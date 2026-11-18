//! Unattended start (#1182): a bot host has nobody to type a PIN, so its keys
//! sit in the device keystore instead of under one.
//!
//! One keystore slot per bot ([`INSTALL_SLOT`], per-user like every slot)
//! holds the bot's account seed, its local-DB key, its device id and its group.
//! The keystore backend is the OS keychain where one exists and the
//! machine-bound encrypted file where not — never plaintext (`keystore.rs`).
//! That resists a copied disk or volume, not a same-UID local attacker
//! (`docs/security-whitepaper.md` §3.5).
//!
//! Only the bot pairing path writes the slot, and only after the DS has said
//! the account is a bot and the host has certified its device. [`start_unattended`]
//! refuses an account that has a PIN, an account with no slot, and an account
//! the DS does not report as a bot — so a human account can never take this path.

use std::sync::Arc;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::commands::pin::UnlockState;
use crate::error::{Error, Result};
use crate::state::AppState;

/// The per-user keystore slot holding an installation.
const INSTALL_SLOT: &str = "bot_installation";
/// Mirrors `commands::pin::PIN_META_SLOT`: present only for an account with a PIN.
const PIN_META_SLOT: &str = "pin_meta";
const RECORD_VERSION: u32 = 1;

/// Every refusal of the unattended path starts with this.
pub const UNATTENDED_REFUSED: &str = "unattended_refused";

/// What a running bot installation is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSession {
    pub bot_user_id: String,
    pub group_id: String,
    pub device_id: String,
}

/// The sealed record. Secrets base64 (STANDARD); the whole is sealed by the keystore.
#[derive(Serialize, Deserialize)]
struct InstallRecord {
    v: u32,
    bot_user_id: String,
    group_id: String,
    device_id: String,
    account_seed: String,
    db_key: String,
}

/// The secrets of an installation, in memory.
pub(crate) struct Installation {
    pub session: BotSession,
    pub account_seed: Zeroizing<Vec<u8>>,
    pub db_key: Zeroizing<Vec<u8>>,
}

fn refused(why: &str) -> Error {
    Error::Other(anyhow::anyhow!("{UNATTENDED_REFUSED}: {why}"))
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Write the installation to the keystore. Pairing only.
pub(crate) async fn persist(state: &AppState, inst: &Installation) -> Result<()> {
    let record = InstallRecord {
        v: RECORD_VERSION,
        bot_user_id: inst.session.bot_user_id.clone(),
        group_id: inst.session.group_id.clone(),
        device_id: inst.session.device_id.clone(),
        account_seed: b64().encode(&*inst.account_seed),
        db_key: b64().encode(&*inst.db_key),
    };
    let bytes = Zeroizing::new(
        serde_json::to_vec(&record).map_err(|e| Error::Other(anyhow::anyhow!("installation record: {e}")))?,
    );
    let InstallRecord { account_seed, db_key, .. } = record;
    drop(Zeroizing::new(account_seed));
    drop(Zeroizing::new(db_key));
    state
        .keystore
        .store_for_user(INSTALL_SLOT, &inst.session.bot_user_id, &bytes)
        .await
}

async fn load(state: &AppState, bot_user_id: &str) -> Result<Option<Installation>> {
    let Some(bytes) = state.keystore.load_for_user(INSTALL_SLOT, bot_user_id).await? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    let record: InstallRecord =
        serde_json::from_slice(&bytes).map_err(|_| refused("the installation record is unreadable"))?;
    let account_seed = Zeroizing::new(record.account_seed);
    let db_key = Zeroizing::new(record.db_key);
    if record.v != RECORD_VERSION || record.bot_user_id != bot_user_id {
        return Err(refused("the installation record is not this bot's"));
    }
    let decode = |s: &str| b64().decode(s).map(Zeroizing::new).map_err(|_| refused("the installation record is unreadable"));
    Ok(Some(Installation {
        session: BotSession {
            bot_user_id: record.bot_user_id,
            group_id: record.group_id,
            device_id: record.device_id,
        },
        account_seed: decode(&account_seed)?,
        db_key: decode(&db_key)?,
    }))
}

/// Whether this keystore holds an installation for `bot_user_id`.
pub async fn has_installation(state: &AppState, bot_user_id: &str) -> Result<bool> {
    Ok(state.keystore.load_for_user(INSTALL_SLOT, bot_user_id).await?.is_some())
}

/// Make `inst` this state's signed-in identity: device id, account key, local DB.
/// Returns whether the local MLS store was empty.
pub(crate) async fn activate(state: &Arc<AppState>, inst: &Installation) -> Result<bool> {
    let user_id = &inst.session.bot_user_id;
    *state.device_id.lock().await = Some(inst.session.device_id.clone());
    *state.unlock.lock().await = Some(UnlockState {
        user_id: user_id.clone(),
        db_key: Zeroizing::new(inst.db_key.to_vec()),
        account_id_key: Zeroizing::new(inst.account_seed.to_vec()),
    });
    state.load_user_db_with_key(user_id, &inst.db_key).await
}

/// Undo [`activate`]: close the DB and drop the in-memory keys.
pub(crate) async fn deactivate(state: &AppState) {
    state.unload_user_db().await;
    *state.unlock.lock().await = None;
    *state.device_id.lock().await = None;
}

/// Start a bot installation with no PIN, from the device keystore alone.
///
/// Refuses (error starting with [`UNATTENDED_REFUSED`]) an account with a PIN,
/// an account with no installation slot, and one the DS does not report as a
/// bot. The caller then catches the installation up (`join_bot_group`, the
/// ordinary MLS catch-up) before handling triggers.
pub async fn start_unattended(state: &Arc<AppState>, bot_user_id: &str) -> Result<BotSession> {
    if state.keystore.load_for_user(PIN_META_SLOT, bot_user_id).await?.is_some() {
        return Err(refused("this account has a PIN, so it is a person's and cannot start unattended"));
    }
    let Some(inst) = load(state, bot_user_id).await? else {
        return Err(refused("no bot installation for this account on this machine"));
    };
    activate(state, &inst).await?;
    match confirm_is_bot(state, bot_user_id).await {
        Ok(()) => Ok(inst.session),
        Err(e) => {
            deactivate(state).await;
            let msg = e.to_string();
            if msg.starts_with(UNATTENDED_REFUSED) {
                Err(e)
            } else {
                Err(refused(&format!("could not confirm the account is a bot: {msg}")))
            }
        }
    }
}

/// The DS's word that the signed-in account is a bot. A DS that lies "human"
/// only stops the bot; a human account never reaches here without a slot.
pub(crate) async fn confirm_is_bot(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let users = crate::commands::ds_reads::users(state, vec![user_id.to_string()], None).await?;
    match users.into_iter().find(|u| u.id == user_id) {
        Some(u) if u.is_bot => Ok(()),
        Some(_) => Err(refused("the account is not a bot")),
        None => Err(refused("the account no longer exists")),
    }
}

/// Seal an installation again after [`start_unattended`]: close its local DB
/// and drop its keys from memory. The bot runtime calls this at the end of
/// every job, so an installation's state is unsealed only while a job runs.
pub async fn stop_unattended(state: &AppState) {
    deactivate(state).await;
}

/// Remove an installation from this machine: its keystore slot and its local DB.
/// For a bot that was deleted or a host that was revoked.
pub async fn forget_installation(state: &Arc<AppState>, bot_user_id: &str) -> Result<()> {
    let active = state.unlock.lock().await.as_ref().is_some_and(|u| u.user_id == bot_user_id);
    if active {
        deactivate(state).await;
    }
    state.keystore.delete_for_user(INSTALL_SLOT, bot_user_id).await?;
    crate::db::local::remove_user_database(&state.local_data_dir(), bot_user_id);
    Ok(())
}
