//! MLS key-package lifecycle commands.
//!
//! These three commands handle the KeyPackage lifecycle that precedes any MLS
//! group operation:
//!
//!   generate_mls_key_package  — create a fresh KeyPackage + SignatureKeyPair,
//!                               persist everything in the local mls_kv table
//!   publish_mls_key_package   — upload the public KeyPackage to the remote
//!                               mls_key_package table in Turso
//!   fetch_mls_key_package     — atomically claim one unclaimed KeyPackage for
//!                               a target user from the remote table
//!
//! Both `generate_mls_key_package` and `publish_mls_key_package` are called
//! from `initialize_identity` so every user has a published package available
//! for use in Phase 3 group/DM creation.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;
use tauri::State;
use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};
use ulid::Ulid;

use crate::error::Result;
use crate::signal::mls_storage::{MlsStore, MlsStorageError};
use crate::state::AppState;

// ── Provider ─────────────────────────────────────────────────────────────────

/// Combines `RustCrypto` with our SQLite-backed `MlsStore` to satisfy the
/// `OpenMlsProvider` bound required by all openmls API calls.
pub struct PollisProvider<'a> {
    crypto: RustCrypto,
    store: MlsStore<'a>,
}

impl<'a> PollisProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            crypto: RustCrypto::default(),
            store: MlsStore::new(conn),
        }
    }
}

impl<'a> OpenMlsProvider for PollisProvider<'a> {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = MlsStore<'a>;

    fn storage(&self) -> &Self::StorageProvider {
        &self.store
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

// ── Ciphersuite ───────────────────────────────────────────────────────────────

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

// ── Error conversions ─────────────────────────────────────────────────────────

impl From<MlsStorageError> for crate::error::Error {
    fn from(e: MlsStorageError) -> Self {
        crate::error::Error::Other(anyhow::anyhow!("MLS storage: {e}"))
    }
}

// ── Credential helpers ───────────────────────────────────────────────────────

/// Build an MLS `Credential` encoding both user and device identity.
///
/// Format: `"user_id:device_id"` as UTF-8 bytes inside a `BasicCredential`.
fn make_credential(user_id: &str, device_id: &str) -> Credential {
    BasicCredential::new(format!("{user_id}:{device_id}").into_bytes()).into()
}

/// Extract the `user_id` from a credential produced by `make_credential`.
///
/// Handles legacy credentials that contain only `user_id` (no colon).
fn parse_credential_user_id(cred: &Credential) -> String {
    let s = String::from_utf8_lossy(cred.serialized_content());
    s.split_once(':').map(|(u, _)| u).unwrap_or(&s).to_string()
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Generate a fresh MLS `KeyPackage` + `SignatureKeyPair` for this device and
/// persist both in the local `mls_kv` table.
///
/// Returns the TLS-serialised `KeyPackage` bytes and its hex-encoded hash ref.
/// Safe to call multiple times — each call produces a distinct key package.
#[tauri::command]
pub async fn generate_mls_key_package(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<serde_json::Value> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;

    // All local DB work is sync — collect results in a block before any await.
    let (ref_hex, kp_bytes) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;

        let provider = PollisProvider::new(db.conn());

        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;

        sig_keys.store(provider.storage())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;

        let credential = make_credential(&user_id, &device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_keys.to_public_vec().into(),
            CS.signature_algorithm(),
        ).map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let bundle = KeyPackage::builder()
            .build(CS, &provider, &sig_keys, cred_with_key)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp build: {e}")))?;

        let kp = bundle.key_package();
        let hash_ref = kp
            .hash_ref(provider.crypto())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp hash_ref: {e}")))?;
        let ref_hex = hex::encode(hash_ref.as_slice());
        let kp_bytes = kp
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp serialize: {e}")))?;

        (ref_hex, kp_bytes)
    };

    Ok(serde_json::json!({ "ref_hex": ref_hex, "key_package_bytes": kp_bytes }))
}

/// Upload a TLS-serialised `KeyPackage` (produced by `generate_mls_key_package`)
/// to the remote `mls_key_package` table so other users can claim it.
#[tauri::command]
pub async fn publish_mls_key_package(
    state: State<'_, Arc<AppState>>,
    user_id: String,
    ref_hex: String,
    key_package_bytes: Vec<u8>,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![ref_hex, user_id, key_package_bytes, device_id],
    ).await?;
    Ok(())
}

/// Atomically claim one unclaimed `KeyPackage` for `target_user_id` from the
/// remote table and return its TLS bytes.  Returns `null` if none are available.
#[tauri::command]
pub async fn fetch_mls_key_package(
    state: State<'_, Arc<AppState>>,
    target_user_id: String,
) -> Result<Option<Vec<u8>>> {
    let conn = state.remote_db.conn().await?;

    // Atomically claim the oldest unclaimed package.
    let mut rows = conn.query(
        "UPDATE mls_key_package
         SET claimed = 1
         WHERE ref_hash = (
             SELECT ref_hash FROM mls_key_package
             WHERE user_id = ?1 AND claimed = 0
             ORDER BY created_at ASC
             LIMIT 1
         )
         RETURNING key_package",
        libsql::params![target_user_id],
    ).await?;

    match rows.next().await? {
        Some(row) => {
            let bytes: Vec<u8> = row.get(0)?;
            Ok(Some(bytes))
        }
        None => Ok(None),
    }
}

/// Rotate key packages: delete unclaimed packages for this device from the
/// remote table and publish TARGET fresh ones backed by the current local DB.
///
/// Called from `initialize_identity` on every login.  Only deletes packages
/// for the current `device_id` — other devices' packages are left intact.
pub async fn ensure_mls_key_package(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    const TARGET: i64 = 5;

    let conn = state.remote_db.conn().await?;

    // Remove unclaimed packages for THIS device only — their private keys may
    // no longer exist in the current local DB (e.g. after a wipe).
    // Also clean up legacy packages with NULL device_id for this user.
    conn.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1 AND claimed = 0 \
         AND (device_id = ?2 OR device_id IS NULL)",
        libsql::params![user_id, device_id],
    ).await?;

    // Generate and publish TARGET fresh packages.
    for _ in 0..TARGET {
        let (ref_hex, kp_bytes) = {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
            })?;
            let provider = PollisProvider::new(db.conn());

            let sig_keys = SignatureKeyPair::new(CS.signature_algorithm())
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;
            sig_keys.store(provider.storage())
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;

            let credential = make_credential(user_id, device_id);
            let sig_pub = OpenMlsSignaturePublicKey::new(
                sig_keys.to_public_vec().into(),
                CS.signature_algorithm(),
            ).map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
            let cred_with_key = CredentialWithKey {
                credential,
                signature_key: sig_pub.into(),
            };

            let bundle = KeyPackage::builder()
                .build(CS, &provider, &sig_keys, cred_with_key)
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp build: {e}")))?;

            let kp = bundle.key_package();
            let hash_ref = kp
                .hash_ref(provider.crypto())
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp hash_ref: {e}")))?;
            let ref_hex = hex::encode(hash_ref.as_slice());
            let kp_bytes = kp
                .tls_serialize_detached()
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp serialize: {e}")))?;

            (ref_hex, kp_bytes)
        };

        conn.execute(
            "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![ref_hex, user_id, kp_bytes, device_id],
        ).await?;
    }

    Ok(())
}

/// Top-up key packages for this device to TARGET without deleting existing ones.
/// Called after processing welcomes (which consume KPs) so the device stays
/// reachable for future group invites.
async fn replenish_key_packages(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    const TARGET: i64 = 5;

    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0",
        libsql::params![user_id, device_id],
    ).await?;
    let remaining: i64 = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        0
    };
    drop(rows);

    let needed = TARGET - remaining;
    if needed <= 0 {
        return Ok(());
    }

    eprintln!("[mls] replenish: {remaining} unclaimed KPs, publishing {needed} more");

    for _ in 0..needed {
        let (ref_hex, kp_bytes) = {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
            })?;
            let provider = PollisProvider::new(db.conn());

            let sig_keys = SignatureKeyPair::new(CS.signature_algorithm())
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;
            sig_keys.store(provider.storage())
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;

            let credential = make_credential(user_id, device_id);
            let sig_pub = OpenMlsSignaturePublicKey::new(
                sig_keys.to_public_vec().into(),
                CS.signature_algorithm(),
            ).map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
            let cred_with_key = CredentialWithKey {
                credential,
                signature_key: sig_pub.into(),
            };

            let bundle = KeyPackage::builder()
                .build(CS, &provider, &sig_keys, cred_with_key)
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp build: {e}")))?;

            let kp = bundle.key_package();
            let hash_ref = kp
                .hash_ref(provider.crypto())
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp hash_ref: {e}")))?;
            let ref_hex = hex::encode(hash_ref.as_slice());
            let kp_bytes = kp
                .tls_serialize_detached()
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp serialize: {e}")))?;

            (ref_hex, kp_bytes)
        };

        conn.execute(
            "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![ref_hex, user_id, kp_bytes, device_id],
        ).await?;
    }

    Ok(())
}

// ── Phase 3: Group / DM creation ─────────────────────────────────────────────

/// Internal: create a fresh MLS group for `conversation_id` with
/// `creator_user_id` as the sole initial member.  Group state is persisted in
/// the local `mls_kv` table via `MlsStore`.
///
/// `use_ratchet_tree_extension(true)` is set so that Welcome messages sent in
/// Phase 4 embed the full ratchet tree — recipients can join without a separate
/// out-of-band tree download.
pub async fn init_mls_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    creator_user_id: &str,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;

    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());

    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;
    sig_keys.store(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;

    let credential = make_credential(creator_user_id, &device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS.signature_algorithm(),
    ).map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());

    // Delete any stale group with the same ID so the create below never
    // collides.  This is a no-op on first creation and essential during
    // repair (where the old group still exists but is broken/outdated).
    if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
        let _ = old.delete(provider.storage());
    }

    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        .use_ratchet_tree_extension(true)
        .build();

    MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create mls group: {e}")))?;

    Ok(())
}

/// Create a fresh MLS group for `conversation_id` (a channel or DM ULID).
/// The creator becomes the sole initial member.  Other users are added via
/// Phase 4 `add_member_mls`.
#[tauri::command]
pub async fn create_mls_group(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    creator_user_id: String,
) -> Result<()> {
    init_mls_group(state.inner(), &conversation_id, &creator_user_id).await
}

/// Internal: deserialise a TLS-encoded `MlsMessageOut` (welcome wire format)
/// and persist the resulting MLS group state locally.
///
/// The bytes stored in `mls_welcome.welcome_data` are TLS-serialised
/// `MlsMessageOut`.  We deserialise to `MlsMessageIn`, extract the inner
/// `Welcome` via `MlsMessageIn::extract()`, then call
/// `StagedWelcome::new_from_welcome`.
pub async fn apply_welcome(state: &Arc<AppState>, welcome_bytes: &[u8]) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());

    let mut reader: &[u8] = welcome_bytes;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("welcome msg deserialize: {e}")))?;

    let welcome = match msg_in.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => return Err(crate::error::Error::Other(anyhow::anyhow!(
            "expected Welcome message in mls_welcome"
        ))),
    };

    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();

    // Split into ProcessedWelcome → delete stale group → stage → into_group.
    // openmls checks for duplicate GroupIds inside `into_staged_welcome`, so we
    // must delete any existing group *before* that call.
    let processed = ProcessedWelcome::new_from_welcome(&provider, &join_config, welcome)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("process welcome: {e}")))?;

    let new_group_id = processed.unverified_group_info().group_id().clone();
    if let Ok(Some(mut old_group)) = MlsGroup::load(provider.storage(), &new_group_id) {
        eprintln!("[mls] apply_welcome: deleting stale group {:?} before re-joining", new_group_id);
        let _ = old_group.delete(provider.storage());
    }

    let staged = processed.into_staged_welcome(&provider, None)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("stage welcome: {e}")))?;

    staged.into_group(&provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("into group: {e}")))?;

    Ok(())
}

/// Process a TLS-encoded MLS `Welcome` and persist the resulting group state.
/// Production code uses `poll_mls_welcomes`; this command is exposed for
/// manual invocation or testing.
#[tauri::command]
pub async fn process_welcome(
    state: State<'_, Arc<AppState>>,
    welcome_bytes: Vec<u8>,
) -> Result<()> {
    apply_welcome(state.inner(), &welcome_bytes).await
}

/// Poll the remote `mls_welcome` table for undelivered Welcome messages
/// addressed to `user_id`.  Each one is applied locally and then marked
/// `delivered = 1` so it is not processed again.
///
/// Called on startup and from `poll_pending_messages`.
pub async fn poll_mls_welcomes_inner(state: &Arc<AppState>, user_id: &str, device_id: &str) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Fetch welcomes targeted at this specific device, plus legacy rows
    // (recipient_device_id IS NULL) from before multi-device was deployed.
    let mut rows = conn.query(
        "SELECT id, welcome_data FROM mls_welcome \
         WHERE recipient_id = ?1 AND delivered = 0 \
         AND (recipient_device_id = ?2 OR recipient_device_id IS NULL) \
         ORDER BY created_at ASC",
        libsql::params![user_id, device_id],
    ).await?;

    // Drain into owned Vec so `rows` is dropped before local-DB awaits below.
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        items.push((id, bytes));
    }
    drop(rows);

    let had_welcomes = !items.is_empty();
    for (id, bytes) in items {
        match apply_welcome(state, &bytes).await {
            Ok(()) => {}
            Err(e) => {
                // Mark as delivered even on failure — the private key for this
                // Welcome was likely orphaned by a DB wipe and will never
                // succeed. The repair mechanism will generate a new Welcome.
                eprintln!("[mls] poll_mls_welcomes: failed to apply welcome {id}: {e}");
            }
        }

        let _ = conn.execute(
            "UPDATE mls_welcome SET delivered = 1 WHERE id = ?1",
            libsql::params![id],
        ).await;
    }

    // Each processed welcome consumed a KP — top back up to TARGET.
    if had_welcomes {
        if let Err(e) = replenish_key_packages(state, user_id, device_id).await {
            eprintln!("[mls] KP replenishment failed (non-fatal): {e}");
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn poll_mls_welcomes(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;
    poll_mls_welcomes_inner(state.inner(), &user_id, &device_id).await
}

// ── Phase 4: Member changes ───────────────────────────────────────────────────

/// Reload an existing MLS group from storage and recover the signer.
///
/// Returns `(MlsGroup, SignatureKeyPair)` ready for use with the provider whose
/// connection was passed to `PollisProvider::new`.
fn load_group_with_signer(
    provider: &PollisProvider<'_>,
    conversation_id: &str,
) -> crate::error::Result<(MlsGroup, SignatureKeyPair)> {
    let group_id = GroupId::from_slice(conversation_id.as_bytes());

    let mut group = MlsGroup::load(provider.storage(), &group_id)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!(
            "MLS group not found for conversation {conversation_id}"
        )))?;

    // Retrieve the signature public key stored in the group's leaf node, then
    // read back the full keypair from mls_kv.
    let sig_pub_bytes = group
        .own_leaf_node()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("no own leaf node")))?
        .signature_key()
        .as_slice()
        .to_vec();

    let signer = SignatureKeyPair::read(
        provider.storage(),
        &sig_pub_bytes,
        CS.signature_algorithm(),
    )
    .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("signer not found in mls_kv")))?;

    // Resolve any in-flight pending commit so the group is operational before
    // the caller performs new operations.
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge pending: {e}")))?;

    Ok((group, signer))
}

/// Add `target_user_id` to the MLS group for `conversation_id`.
///
/// Flow:
///   1. Claim the target's unclaimed KeyPackage from the remote table.
///   2. Load the local MLS group + signer.
///   3. Call `MlsGroup::add_members` → (commit, welcome).
///   4. Serialize and merge the commit locally.
///   5. Post the commit to `mls_commit_log` (other members apply it via
///      `process_pending_commits`).
///   6. Post the Welcome to `mls_welcome` (target picks it up via
///      `poll_mls_welcomes`).
pub async fn add_member_mls_inner(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_user_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<()> {
    add_member_mls_impl(state, conversation_id, target_user_id, actor_user_id, None).await
}

#[tauri::command]
pub async fn add_member_mls(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    target_user_id: String,
    actor_user_id: String,
) -> crate::error::Result<()> {
    add_member_mls_impl(state.inner(), &conversation_id, &target_user_id, &actor_user_id, None).await
}

async fn add_member_mls_impl(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_user_id: &str,
    actor_user_id: &str,
    exclude_device_id: Option<&str>,
) -> crate::error::Result<()> {
    let conversation_id = conversation_id.to_owned();
    let target_user_id = target_user_id.to_owned();
    let actor_user_id = actor_user_id.to_owned();

    // 1. Look up all registered devices for the target user.
    let device_ids: Vec<String> = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT device_id FROM user_device WHERE user_id = ?1",
            libsql::params![target_user_id.clone()],
        ).await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            let did: String = row.get(0)?;
            // Skip the excluded device (e.g. the group creator during repair).
            if exclude_device_id.map_or(false, |ex| ex == did) {
                continue;
            }
            ids.push(did);
        }
        ids
    };

    if device_ids.is_empty() {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "No registered devices for {target_user_id}"
        )));
    }

    // 2. Claim one KeyPackage per device.
    let mut kp_bytes_list: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let conn = state.remote_db.conn().await?;
        for did in &device_ids {
            let mut rows = conn.query(
                "UPDATE mls_key_package \
                 SET claimed = 1 \
                 WHERE ref_hash = ( \
                     SELECT ref_hash FROM mls_key_package \
                     WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 \
                     ORDER BY created_at ASC LIMIT 1 \
                 ) \
                 RETURNING key_package",
                libsql::params![target_user_id.clone(), did.clone()],
            ).await?;
            if let Some(row) = rows.next().await? {
                kp_bytes_list.push((did.clone(), row.get::<Vec<u8>>(0)?));
            } else {
                eprintln!("[mls] add_member: no key package for {target_user_id} device {did} — skipping");
            }
        }
    }

    if kp_bytes_list.is_empty() {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "No available key packages for any device of {target_user_id}"
        )));
    }

    // 3. Validate all KPs, load group, create single commit with all devices.
    let (commit_bytes, welcome_bytes, epoch, added_device_ids): (Vec<u8>, Vec<u8>, u64, Vec<String>) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());

        let mut validated_kps: Vec<KeyPackage> = Vec::new();
        let mut added_devs: Vec<String> = Vec::new();
        for (did, kp_raw) in &kp_bytes_list {
            let mut kp_reader: &[u8] = kp_raw;
            let kp_in = match KeyPackageIn::tls_deserialize(&mut kp_reader) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("[mls] add_member: kp deserialize failed for device {did}: {e}");
                    continue;
                }
            };
            let kp = match kp_in.validate(provider.crypto(), ProtocolVersion::Mls10) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("[mls] add_member: kp validate failed for device {did}: {e}");
                    continue;
                }
            };
            // Verify the credential belongs to the target user.
            let cred_user = parse_credential_user_id(kp.leaf_node().credential());
            if cred_user != target_user_id {
                eprintln!("[mls] add_member: credential user '{cred_user}' != '{target_user_id}' for device {did}");
                continue;
            }
            validated_kps.push(kp);
            added_devs.push(did.clone());
        }

        if validated_kps.is_empty() {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "No valid key packages for {target_user_id}"
            )));
        }

        let (mut group, signer) = load_group_with_signer(&provider, &conversation_id)?;
        let epoch = group.epoch().as_u64();

        let (commit_msg, welcome_msg, _group_info) = group
            .add_members(&provider, &signer, &validated_kps)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("add_members: {e}")))?;

        let commit_bytes: Vec<u8> = commit_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("commit serialize: {e}")))?;

        let welcome_bytes: Vec<u8> = welcome_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("welcome serialize: {e}")))?;

        group
            .merge_pending_commit(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge commit: {e}")))?;

        (commit_bytes, welcome_bytes, epoch, added_devs)
    };

    // 4. Post commit to remote.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            conversation_id.clone(),
            epoch as i64,
            actor_user_id,
            commit_bytes
        ],
    ).await?;

    // 5. Post one welcome row per device — same Welcome blob, each device
    //    processes it independently with its own KeyPackage private key.
    for did in &added_device_ids {
        let welcome_id = Ulid::new().to_string();
        conn.execute(
            "INSERT INTO mls_welcome (id, conversation_id, recipient_id, recipient_device_id, welcome_data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![welcome_id, conversation_id.clone(), target_user_id.clone(), did.clone(), welcome_bytes.clone()],
        ).await?;
    }

    Ok(())
}

/// Remove `target_user_id` from the MLS group for `conversation_id`.
///
/// Creates a Remove commit and posts it to `mls_commit_log`.  Remaining
/// members apply it via `process_pending_commits`, which advances the epoch
/// and rotates keys — providing forward secrecy from the removed member.
pub async fn remove_member_mls_inner(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_user_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<()> {
    remove_member_mls_impl(state, conversation_id, target_user_id, actor_user_id).await
}

/// Wipe all local MLS state for a group without publishing a commit.
///
/// Used when the local user leaves a group.  MLS does not allow a member to
/// commit their own removal (`remove_members` with self as target errors), so
/// instead we just delete the local group epoch.  The remaining members still
/// have this user in their group state until the next admin-issued commit, but
/// forward secrecy ensures the leaver cannot decrypt messages after the next
/// epoch advance.
pub async fn forget_local_mls_group(
    state: &Arc<AppState>,
    group_id: &str,
) -> crate::error::Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());
    let mls_group_id = GroupId::from_slice(group_id.as_bytes());

    if let Ok(Some(mut group)) = MlsGroup::load(provider.storage(), &mls_group_id) {
        group.delete(provider.storage())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls delete group: {e}")))?;
    }
    // If the group wasn't found locally, nothing to clean up.
    Ok(())
}

#[tauri::command]
pub async fn remove_member_mls(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    target_user_id: String,
    actor_user_id: String,
) -> crate::error::Result<()> {
    remove_member_mls_impl(state.inner(), &conversation_id, &target_user_id, &actor_user_id).await
}

async fn remove_member_mls_impl(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_user_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<()> {
    let conversation_id = conversation_id.to_owned();
    let target_user_id = target_user_id.to_owned();
    let actor_user_id = actor_user_id.to_owned();
    let (commit_bytes, epoch) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        let (mut group, signer) = load_group_with_signer(&provider, &conversation_id)?;
        let epoch = group.epoch().as_u64();

        // Find ALL leaf indices belonging to the target user (may have
        // multiple devices, each with its own leaf node).
        let leaf_indices: Vec<LeafNodeIndex> = group.members()
            .filter_map(|m| {
                let cred_user = parse_credential_user_id(&m.credential);
                if cred_user == target_user_id {
                    Some(m.index)
                } else {
                    None
                }
            })
            .collect();

        if leaf_indices.is_empty() {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "'{target_user_id}' is not a member of group {conversation_id}"
            )));
        }

        let (commit_msg, _welcome, _group_info) = group
            .remove_members(&provider, &signer, &leaf_indices)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("remove_members: {e}")))?;

        let commit_bytes = commit_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("commit serialize: {e}")))?;

        group
            .merge_pending_commit(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge commit: {e}")))?;

        (commit_bytes, epoch)
    };

    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            conversation_id,
            epoch as i64,
            actor_user_id,
            commit_bytes
        ],
    ).await?;

    Ok(())
}

/// Apply any commits from `mls_commit_log` that this member has not yet seen.
///
/// Reads rows where `epoch >= current_local_epoch` in ascending order, applies
/// each commit, and advances the local epoch.  An epoch gap (unexpected jump)
/// stops processing and logs an error — this indicates a missed or reordered
/// commit that would require manual intervention in a production system.
///
/// `mls_group_id` must already be resolved (group_id for channels,
/// conversation_id for DMs).
pub async fn process_pending_commits_inner(
    state: &Arc<AppState>,
    mls_group_id: &str,
) -> crate::error::Result<()> {
    // 1. Get the current epoch from the local group.
    let initial_epoch = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(mls_group_id.as_bytes());
        let group = match MlsGroup::load(provider.storage(), &group_id) {
            Ok(Some(g)) => g,
            _ => {
                // Group doesn't exist locally (e.g. DB was wiped). Nothing to
                // process — repair will happen on first send.
                return Ok(());
            }
        };
        group.epoch().as_u64()
    };

    // 2. Fetch pending commits from remote, collected into an owned Vec so
    //    the `rows` cursor is dropped before any local-DB await below.
    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT epoch, commit_data \
         FROM mls_commit_log \
         WHERE conversation_id = ?1 AND epoch >= ?2 \
         ORDER BY epoch ASC, seq ASC",
        libsql::params![mls_group_id, initial_epoch as i64],
    ).await?;

    let mut pending: Vec<(i64, Vec<u8>)> = Vec::new();
    while let Some(row) = rows.next().await? {
        let epoch: i64 = row.get(0)?;
        let data: Vec<u8> = row.get(1)?;
        pending.push((epoch, data));
    }
    drop(rows);

    // 3. Apply each commit in epoch order.
    let mut current_epoch = initial_epoch;
    for (row_epoch, commit_data) in pending {
        if row_epoch as u64 != current_epoch {
            eprintln!(
                "[mls] process_pending_commits: epoch gap for {mls_group_id}: \
                 expected {current_epoch}, got {row_epoch} — stopping"
            );
            break;
        }

        // All MLS work is synchronous and scoped so nothing !Send crosses
        // the lock().await boundary.
        let applied = {
            let guard = state.local_db.lock().await;
            let db = match guard.as_ref() {
                Some(db) => db,
                None => break,
            };
            let provider = PollisProvider::new(db.conn());
            let group_id = GroupId::from_slice(mls_group_id.as_bytes());
            let mut group = match MlsGroup::load(provider.storage(), &group_id)
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?
            {
                Some(g) => g,
                None => break,
            };

            let mut reader: &[u8] = &commit_data;
            let msg_in = match MlsMessageIn::tls_deserialize(&mut reader) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[mls] process_pending_commits: deserialize failed for {mls_group_id}: {e} — deleting stale group");
                    let _ = group.delete(provider.storage());
                    return Ok(());
                }
            };
            let protocol_msg = match msg_in.try_into_protocol_message() {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[mls] process_pending_commits: protocol msg failed for {mls_group_id}: {e} — deleting stale group");
                    let _ = group.delete(provider.storage());
                    return Ok(());
                }
            };

            match group.process_message(&provider, protocol_msg) {
                Ok(processed) => {
                    if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
                        if let Err(e) = group.merge_staged_commit(&provider, *staged) {
                            eprintln!("[mls] process_pending_commits: merge failed for {mls_group_id}: {e} — deleting stale group");
                            let _ = group.delete(provider.storage());
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    // AEAD or epoch mismatch — local state is unrecoverable.
                    // Delete the group so repair recreates it on next send.
                    eprintln!("[mls] process_pending_commits: {e} for {mls_group_id} — deleting stale group");
                    let _ = group.delete(provider.storage());
                    return Ok(());
                }
            }

            true
        };

        if applied {
            current_epoch += 1;
        }
    }

    Ok(())
}

/// Tauri command wrapper — resolves conversation_id to MLS group ID, then
/// delegates to `process_pending_commits_inner`.
#[tauri::command]
pub async fn process_pending_commits(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> crate::error::Result<()> {
    let mls_group_id = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![conversation_id.clone()],
        ).await?;
        match rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => conversation_id,
        }
    };
    process_pending_commits_inner(state.inner(), &mls_group_id).await
}

// ── Phase 4.5: MLS group self-repair ─────────────────────────────────────────

/// Re-create the MLS group for a channel and re-add all current members.
///
/// Called transparently when `try_mls_encrypt` returns `None` (i.e. the local
/// MLS state was lost — typically after a local DB schema bump wiped the file).
/// The sender becomes the new group "creator"; fresh Welcome messages are
/// generated for every other member using their latest key packages.
///
/// Members who haven't logged in since the wipe won't have key packages yet —
/// they're silently skipped and will be repaired the next time *they* send.
pub async fn repair_mls_group(
    state: &Arc<AppState>,
    mls_group_id: &str,
    sender_id: &str,
) -> crate::error::Result<()> {
    eprintln!("[mls] repair: re-creating MLS group {mls_group_id}");

    // 1. Create a fresh MLS group with the sender as sole member.
    init_mls_group(state, mls_group_id, sender_id).await?;

    // 1b. Purge stale commit log and welcome entries so process_pending_commits
    //     doesn't try to apply old-generation commits against the new group.
    let conn = state.remote_db.conn().await?;
    let _ = conn.execute(
        "DELETE FROM mls_commit_log WHERE conversation_id = ?1",
        libsql::params![mls_group_id],
    ).await;
    // Only delete welcomes for THIS conversation — not other conversations
    // the sender may have pending welcomes for.
    let _ = conn.execute(
        "DELETE FROM mls_welcome WHERE conversation_id = ?1 AND delivered = 0",
        libsql::params![mls_group_id],
    ).await;
    drop(conn);

    // 2. Look up all members (including sender — their other devices need
    //    to be added too). Group channels use `group_member`; DM conversations
    //    use `dm_channel_member`. Try groups first, fall back to DMs.
    let conn = state.remote_db.conn().await?;
    let mut member_ids: Vec<String> = Vec::new();
    {
        let mut rows = conn.query(
            "SELECT user_id FROM group_member WHERE group_id = ?1",
            libsql::params![mls_group_id],
        ).await?;
        while let Some(row) = rows.next().await? {
            member_ids.push(row.get::<String>(0)?);
        }
    }
    if member_ids.is_empty() {
        let mut rows = conn.query(
            "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1",
            libsql::params![mls_group_id],
        ).await?;
        while let Some(row) = rows.next().await? {
            member_ids.push(row.get::<String>(0)?);
        }
    }

    // 3. Collect ALL key packages for ALL members' devices in one pass,
    //    then add them in a single `add_members` call — one commit, one epoch
    //    advance, instead of M separate commits.
    let current_device_id = state.device_id.lock().await.clone();

    // (user_id, device_id, kp_bytes) for each device we successfully claim a KP for.
    let mut claimed_kps: Vec<(String, String, Vec<u8>)> = Vec::new();

    for member_id in &member_ids {
        // Look up all devices for this member.
        let mut device_ids: Vec<String> = Vec::new();
        {
            let mut rows = conn.query(
                "SELECT device_id FROM user_device WHERE user_id = ?1",
                libsql::params![member_id.clone()],
            ).await?;
            while let Some(row) = rows.next().await? {
                let did: String = row.get(0)?;
                // Exclude the sender's current device (already the group creator).
                if member_id == sender_id && current_device_id.as_deref() == Some(did.as_str()) {
                    continue;
                }
                device_ids.push(did);
            }
        }

        // Claim one KP per device.
        for did in &device_ids {
            let mut rows = conn.query(
                "UPDATE mls_key_package \
                 SET claimed = 1 \
                 WHERE ref_hash = ( \
                     SELECT ref_hash FROM mls_key_package \
                     WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 \
                     ORDER BY created_at ASC LIMIT 1 \
                 ) \
                 RETURNING key_package",
                libsql::params![member_id.clone(), did.clone()],
            ).await?;
            if let Some(row) = rows.next().await? {
                claimed_kps.push((member_id.clone(), did.clone(), row.get::<Vec<u8>>(0)?));
            } else {
                eprintln!("[mls] repair: no key package for {member_id} device {did} — skipping");
            }
        }
    }
    drop(conn);

    if claimed_kps.is_empty() {
        eprintln!("[mls] repair: done — no other devices to add");
        return Ok(());
    }

    // 4. Validate all KPs and do a single add_members call.
    let (commit_bytes, welcome_bytes, epoch, welcome_targets): (Vec<u8>, Vec<u8>, u64, Vec<(String, String)>) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());

        let mut validated_kps: Vec<KeyPackage> = Vec::new();
        let mut targets: Vec<(String, String)> = Vec::new();
        for (uid, did, kp_raw) in &claimed_kps {
            let mut kp_reader: &[u8] = kp_raw;
            let kp_in = match KeyPackageIn::tls_deserialize(&mut kp_reader) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("[mls] repair: kp deserialize failed for {uid} device {did}: {e}");
                    continue;
                }
            };
            let kp = match kp_in.validate(provider.crypto(), ProtocolVersion::Mls10) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("[mls] repair: kp validate failed for {uid} device {did}: {e}");
                    continue;
                }
            };
            let cred_user = parse_credential_user_id(kp.leaf_node().credential());
            if cred_user != *uid {
                eprintln!("[mls] repair: credential user '{cred_user}' != '{uid}' for device {did}");
                continue;
            }
            validated_kps.push(kp);
            targets.push((uid.clone(), did.clone()));
        }

        if validated_kps.is_empty() {
            eprintln!("[mls] repair: done — no valid key packages");
            return Ok(());
        }

        let (mut group, signer) = load_group_with_signer(&provider, mls_group_id)?;
        let epoch = group.epoch().as_u64();

        let (commit_msg, welcome_msg, _group_info) = group
            .add_members(&provider, &signer, &validated_kps)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("repair add_members: {e}")))?;

        let commit_bytes = commit_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("commit serialize: {e}")))?;
        let welcome_bytes = welcome_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("welcome serialize: {e}")))?;

        group
            .merge_pending_commit(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge commit: {e}")))?;

        (commit_bytes, welcome_bytes, epoch, targets)
    };

    // 5. Post single commit + per-device welcome rows.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![mls_group_id, epoch as i64, sender_id, commit_bytes],
    ).await?;

    for (uid, did) in &welcome_targets {
        let welcome_id = Ulid::new().to_string();
        conn.execute(
            "INSERT INTO mls_welcome (id, conversation_id, recipient_id, recipient_device_id, welcome_data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![welcome_id, mls_group_id, uid.clone(), did.clone(), welcome_bytes.clone()],
        ).await?;
    }

    eprintln!("[mls] repair: done — {} devices added in 1 commit", welcome_targets.len());
    Ok(())
}

// ── Phase 5 helpers: encrypt / decrypt ───────────────────────────────────────

/// Check whether an MLS group exists in the local database.
pub fn has_local_group(conn: &rusqlite::Connection, conversation_id: &str) -> bool {
    let provider = PollisProvider::new(conn);
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    matches!(MlsGroup::load(provider.storage(), &group_id), Ok(Some(_)))
}

/// Try to encrypt `plaintext` with the MLS group for `conversation_id`.
///
/// Returns `None` — without logging — if the group does not exist locally
/// (e.g. the channel was created before MLS was rolled out).  The caller
/// should fall back to the legacy Signal sender-key path in that case.
pub fn try_mls_encrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let provider = PollisProvider::new(conn);
    let (mut group, signer) = load_group_with_signer(&provider, conversation_id).ok()?;
    let msg_out = group.create_message(&provider, &signer, plaintext).ok()?;
    msg_out.tls_serialize_detached().ok()
}

/// Try to decrypt MLS ciphertext bytes for `conversation_id`.
///
/// The bytes must be TLS-serialised `MlsMessageOut` (i.e. what we stored in
/// `message_envelope.ciphertext` after `send_message` used MLS).  Returns
/// the raw plaintext bytes on success, or `None` if the bytes are not a
/// valid MLS `ApplicationMessage` or if decryption fails for any reason.
pub fn try_mls_decrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    ciphertext: &[u8],
) -> Option<Vec<u8>> {
    let provider = PollisProvider::new(conn);
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let mut group = MlsGroup::load(provider.storage(), &group_id).ok()??;

    let mut reader: &[u8] = ciphertext;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
    let protocol_msg = msg_in.try_into_protocol_message().ok()?;
    let processed = group.process_message(&provider, protocol_msg).ok()?;

    match processed.into_content() {
        ProcessedMessageContent::ApplicationMessage(app_msg) => Some(app_msg.into_bytes()),
        _ => None,
    }
}

// ── Phase 2 (retained): validate_key_package ─────────────────────────────────

/// Validate that a `KeyPackage` blob received from the remote table is
/// well-formed and matches the expected user's credential.
///
/// Returns the hex-encoded `KeyPackageRef` on success so callers can store it.
pub fn validate_key_package(
    kp_bytes: &[u8],
    expected_user_id: &str,
    crypto: &RustCrypto,
) -> Result<String> {
    let mut reader: &[u8] = kp_bytes;
    let kp_in = KeyPackageIn::tls_deserialize(&mut reader)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp deserialize: {e}")))?;

    let kp = kp_in
        .validate(crypto, ProtocolVersion::Mls10)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp validate: {e}")))?;

    // Verify the credential user_id matches the expected user.
    let cred_user = parse_credential_user_id(kp.leaf_node().credential());
    if cred_user != expected_user_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "KeyPackage credential user '{cred_user}' does not match expected '{expected_user_id}'"
        )));
    }

    let hash_ref = kp
        .hash_ref(crypto)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp hash_ref: {e}")))?;

    Ok(hex::encode(hash_ref.as_slice()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tls_codec::Serialize as TlsSerialize;

    /// Create an in-memory SQLite DB with the `mls_kv` table.
    fn make_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mls_kv (
                scope TEXT NOT NULL,
                key   BLOB NOT NULL,
                value BLOB NOT NULL,
                PRIMARY KEY (scope, key)
            );",
        ).unwrap();
        conn
    }

    /// Synthetic device ID for test users. In tests each "user" maps to a
    /// single device so we derive a deterministic device_id from the user_id.
    fn test_device_id(user_id: &str) -> String {
        format!("{user_id}_dev")
    }

    /// Create an MLS group with `user_id` as sole member and return the
    /// `SignatureKeyPair` so the caller can later call `create_message`.
    fn create_group(
        conn: &rusqlite::Connection,
        conversation_id: &str,
        user_id: &str,
    ) -> SignatureKeyPair {
        let provider = PollisProvider::new(conn);
        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        sig_keys.store(provider.storage()).unwrap();

        let credential = make_credential(user_id, &test_device_id(user_id));
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_keys.to_public_vec().into(),
            CS.signature_algorithm(),
        ).unwrap();
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CS)
            .use_ratchet_tree_extension(true)
            .build();

        MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
            .unwrap();

        sig_keys
    }

    /// Generate a key package for `user_id` in `conn` and return the TLS bytes.
    fn gen_key_package(conn: &rusqlite::Connection, user_id: &str) -> Vec<u8> {
        let provider = PollisProvider::new(conn);
        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        sig_keys.store(provider.storage()).unwrap();

        let credential = make_credential(user_id, &test_device_id(user_id));
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_keys.to_public_vec().into(),
            CS.signature_algorithm(),
        ).unwrap();
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let bundle = KeyPackage::builder()
            .build(CS, &provider, &sig_keys, cred_with_key)
            .unwrap();

        bundle.key_package().tls_serialize_detached().unwrap()
    }

    /// Alice creates a group, adds Bob, then Alice encrypts and Bob decrypts.
    #[test]
    fn encrypt_decrypt_roundtrip() {
        let conv_id = "01JTEST00000000000000000AB";

        let alice_db = make_db();
        let bob_db = make_db();

        // Alice creates the group.
        create_group(&alice_db, conv_id, "alice");

        // Bob generates a key package.
        let bob_kp_bytes = gen_key_package(&bob_db, "bob");

        // Alice adds Bob: add_members → commit + welcome.
        let welcome_bytes: Vec<u8> = {
            let alice_provider = PollisProvider::new(&alice_db);
            let (mut alice_group, alice_signer) =
                load_group_with_signer(&alice_provider, conv_id).unwrap();

            let mut kp_reader: &[u8] = &bob_kp_bytes;
            let kp_in = KeyPackageIn::tls_deserialize(&mut kp_reader).unwrap();
            let kp = kp_in.validate(alice_provider.crypto(), ProtocolVersion::Mls10).unwrap();

            let (commit_msg, welcome_msg, _) =
                alice_group.add_members(&alice_provider, &alice_signer, &[kp]).unwrap();

            alice_group.merge_pending_commit(&alice_provider).unwrap();

            // Keep commit_msg in scope to avoid "unused" warn — it would be posted
            // to mls_commit_log in production.
            let _ = commit_msg.tls_serialize_detached().unwrap();
            welcome_msg.tls_serialize_detached().unwrap()
        };

        // Bob processes the Welcome.
        {
            let bob_provider = PollisProvider::new(&bob_db);
            let mut reader: &[u8] = &welcome_bytes;
            let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
            let welcome = match msg_in.extract() {
                MlsMessageBodyIn::Welcome(w) => w,
                _ => panic!("expected Welcome"),
            };
            let join_config = MlsGroupJoinConfig::default();
            StagedWelcome::new_from_welcome(&bob_provider, &join_config, welcome, None)
                .unwrap()
                .into_group(&bob_provider)
                .unwrap();
        }

        // Alice encrypts.
        let plaintext = b"hello mls";
        let ciphertext = try_mls_encrypt(&alice_db, conv_id, plaintext)
            .expect("try_mls_encrypt failed");

        // Bob decrypts.
        let decrypted = try_mls_decrypt(&bob_db, conv_id, &ciphertext)
            .expect("try_mls_decrypt failed");

        assert_eq!(decrypted, plaintext);
    }

    /// A solo group (no other members) returns None for decrypt — only a member
    /// of the group can decrypt.  But the creator can still encrypt successfully.
    #[test]
    fn solo_group_encrypt_returns_some() {
        let conv_id = "01JTEST00000000000000000CD";
        let alice_db = make_db();
        create_group(&alice_db, conv_id, "alice");

        let ct = try_mls_encrypt(&alice_db, conv_id, b"test");
        assert!(ct.is_some(), "creator should be able to encrypt in a solo group");
    }

    /// A missing group returns None without panicking.
    #[test]
    fn missing_group_returns_none() {
        let db = make_db();
        let ct = try_mls_encrypt(&db, "no-such-group", b"test");
        assert!(ct.is_none());

        let result = try_mls_decrypt(&db, "no-such-group", b"\x00\x01\x02");
        assert!(result.is_none());
    }

    // ── helpers shared by scenario tests ─────────────────────────────────────

    /// Alice adds a member to her group. Returns (commit_bytes, welcome_bytes).
    fn add_member_to_group(
        adder_db: &rusqlite::Connection,
        conv_id: &str,
        kp_bytes: &[u8],
    ) -> (Vec<u8>, Vec<u8>) {
        let provider = PollisProvider::new(adder_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

        let mut reader: &[u8] = kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
        let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();

        let (commit_msg, welcome_msg, _) =
            group.add_members(&provider, &signer, &[kp]).unwrap();
        group.merge_pending_commit(&provider).unwrap();

        (
            commit_msg.tls_serialize_detached().unwrap(),
            welcome_msg.tls_serialize_detached().unwrap(),
        )
    }

    /// Join a group by applying a serialised Welcome message.
    fn join_via_welcome(joiner_db: &rusqlite::Connection, welcome_bytes: &[u8]) {
        let provider = PollisProvider::new(joiner_db);
        let mut reader: &[u8] = welcome_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let welcome = match msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected Welcome"),
        };
        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        StagedWelcome::new_from_welcome(&provider, &join_config, welcome, None)
            .unwrap()
            .into_group(&provider)
            .unwrap();
    }

    /// Apply a serialised commit to advance a member's epoch.
    fn apply_commit(member_db: &rusqlite::Connection, conv_id: &str, commit_bytes: &[u8]) {
        let provider = PollisProvider::new(member_db);
        let group_id = GroupId::from_slice(conv_id.as_bytes());
        let mut group = MlsGroup::load(provider.storage(), &group_id)
            .unwrap()
            .expect("group must exist");

        let mut reader: &[u8] = commit_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let protocol_msg = msg_in.try_into_protocol_message().unwrap();
        let processed = group.process_message(&provider, protocol_msg).unwrap();
        if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
            group.merge_staged_commit(&provider, *staged).unwrap();
        }
    }

    /// Remove a member from the group. Returns the commit bytes (for remaining
    /// members to apply via `apply_commit`).
    fn remove_member(
        remover_db: &rusqlite::Connection,
        conv_id: &str,
        target_user_id: &str,
    ) -> Vec<u8> {
        let provider = PollisProvider::new(remover_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

        // Find all leaves for the target user (may have multiple devices).
        let leaf_indices: Vec<LeafNodeIndex> = group.members()
            .filter_map(|m| {
                let cred_user = parse_credential_user_id(&m.credential);
                if cred_user == target_user_id {
                    Some(m.index)
                } else {
                    None
                }
            })
            .collect();
        assert!(!leaf_indices.is_empty(), "target must be in group");

        let (commit_msg, _, _) =
            group.remove_members(&provider, &signer, &leaf_indices).unwrap();
        group.merge_pending_commit(&provider).unwrap();

        commit_msg.tls_serialize_detached().unwrap()
    }

    // ── scenario tests ────────────────────────────────────────────────────────

    /// Alice, Bob, and Carol are all in the same group. Each can encrypt a
    /// message that the other two can decrypt.
    #[test]
    fn three_way_group_messaging() {
        let conv_id = "01JTEST00000000000000000EF";

        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();

        // Alice creates the group.
        create_group(&alice_db, conv_id, "alice");

        // Alice adds Bob.
        let bob_kp = gen_key_package(&bob_db, "bob");
        let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Alice adds Carol. Bob must also apply this commit.
        let carol_kp = gen_key_package(&carol_db, "carol");
        let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
        join_via_welcome(&carol_db, &carol_welcome);
        apply_commit(&bob_db, conv_id, &add_carol_commit);

        // Suppress unused-variable warning — add_bob_commit would go to mls_commit_log in prod.
        let _ = add_bob_commit;

        // Alice sends → Bob and Carol both decrypt.
        let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"hello from alice").unwrap();
        assert_eq!(try_mls_decrypt(&bob_db, conv_id, &alice_ct).unwrap(), b"hello from alice");
        assert_eq!(try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap(), b"hello from alice");

        // Bob sends → Alice and Carol both decrypt.
        let bob_ct = try_mls_encrypt(&bob_db, conv_id, b"hello from bob").unwrap();
        assert_eq!(try_mls_decrypt(&alice_db, conv_id, &bob_ct).unwrap(), b"hello from bob");
        assert_eq!(try_mls_decrypt(&carol_db, conv_id, &bob_ct).unwrap(), b"hello from bob");

        // Carol sends → Alice and Bob both decrypt.
        let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"hello from carol").unwrap();
        assert_eq!(try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap(), b"hello from carol");
        assert_eq!(try_mls_decrypt(&bob_db, conv_id, &carol_ct).unwrap(), b"hello from carol");
    }

    /// After Bob is removed, messages Alice sends are encrypted at the new epoch.
    /// Bob's local state is stuck at the old epoch, so he cannot decrypt them.
    #[test]
    fn removed_member_cannot_decrypt_new_messages() {
        let conv_id = "01JTEST00000000000000000GH";

        let alice_db = make_db();
        let bob_db = make_db();

        create_group(&alice_db, conv_id, "alice");

        let bob_kp = gen_key_package(&bob_db, "bob");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Confirm Bob can read messages before removal.
        let pre_remove_ct = try_mls_encrypt(&alice_db, conv_id, b"pre-removal").unwrap();
        assert_eq!(
            try_mls_decrypt(&bob_db, conv_id, &pre_remove_ct).unwrap(),
            b"pre-removal"
        );

        // Alice removes Bob. Alice's epoch advances; Bob's does not.
        let _remove_commit = remove_member(&alice_db, conv_id, "bob");

        // Alice sends a message at the new epoch.
        let post_remove_ct = try_mls_encrypt(&alice_db, conv_id, b"secret").unwrap();

        // Bob cannot decrypt it — forward secrecy enforced.
        assert!(
            try_mls_decrypt(&bob_db, conv_id, &post_remove_ct).is_none(),
            "removed member must not decrypt messages from new epoch"
        );
    }

    /// A newly added member cannot decrypt messages that were sent before they joined.
    #[test]
    fn new_member_cannot_read_history() {
        let conv_id = "01JTEST00000000000000000IJ";

        let alice_db = make_db();
        let bob_db = make_db();

        create_group(&alice_db, conv_id, "alice");

        // Alice sends a message before Bob exists.
        let history_ct = try_mls_encrypt(&alice_db, conv_id, b"private history").unwrap();

        // Alice adds Bob.
        let bob_kp = gen_key_package(&bob_db, "bob");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Bob cannot decrypt the pre-join message.
        assert!(
            try_mls_decrypt(&bob_db, conv_id, &history_ct).is_none(),
            "new member must not decrypt history from before they joined"
        );

        // But Bob can decrypt messages sent after he joined.
        let new_ct = try_mls_encrypt(&alice_db, conv_id, b"welcome bob").unwrap();
        assert_eq!(
            try_mls_decrypt(&bob_db, conv_id, &new_ct).unwrap(),
            b"welcome bob"
        );
    }

    /// When a new member is added, all existing members must apply the commit
    /// (epoch advance) before they can send/receive further messages.
    #[test]
    fn epoch_sync_via_commit_processing() {
        let conv_id = "01JTEST00000000000000000KL";

        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();

        create_group(&alice_db, conv_id, "alice");

        // Alice adds Bob (epoch 0→1).
        let bob_kp = gen_key_package(&bob_db, "bob");
        let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Bob adds Carol (epoch 1→2). Alice hasn't applied this commit yet.
        let carol_kp = gen_key_package(&carol_db, "carol");
        let (add_carol_commit, carol_welcome) = add_member_to_group(&bob_db, conv_id, &carol_kp);
        join_via_welcome(&carol_db, &carol_welcome);

        // Alice applies Bob's add-Carol commit to advance to epoch 2.
        apply_commit(&alice_db, conv_id, &add_carol_commit);

        let _ = add_bob_commit;

        // Now all three members are at epoch 2 and can communicate.
        let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"carol here").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap(),
            b"carol here"
        );
        assert_eq!(
            try_mls_decrypt(&bob_db, conv_id, &carol_ct).unwrap(),
            b"carol here"
        );
    }

    /// When a member leaves (is removed), the remaining members can still
    /// communicate, the removed member cannot decrypt, and a newly added
    /// member can participate in the group.
    #[test]
    fn leave_group_remaining_members_communicate_then_new_member_joins() {
        let conv_id = "01JTEST0000000000000LEAVE1";

        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();
        let dave_db = make_db();

        // Alice creates group, adds Bob and Carol.
        create_group(&alice_db, conv_id, "alice");

        let bob_kp = gen_key_package(&bob_db, "bob");
        let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);
        let _ = add_bob_commit;

        let carol_kp = gen_key_package(&carol_db, "carol");
        let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
        join_via_welcome(&carol_db, &carol_welcome);
        apply_commit(&bob_db, conv_id, &add_carol_commit);

        // Verify all three can communicate before removal.
        let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"all three here").unwrap();
        assert_eq!(try_mls_decrypt(&bob_db, conv_id, &pre_ct).unwrap(), b"all three here");
        assert_eq!(try_mls_decrypt(&carol_db, conv_id, &pre_ct).unwrap(), b"all three here");

        // Alice removes Bob. Carol applies the commit.
        let remove_bob_commit = remove_member(&alice_db, conv_id, "bob");
        apply_commit(&carol_db, conv_id, &remove_bob_commit);

        // Alice and Carol can still communicate.
        let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
        assert_eq!(try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap(), b"bob is gone");

        let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"confirmed").unwrap();
        assert_eq!(try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap(), b"confirmed");

        // Bob cannot decrypt post-removal messages.
        assert!(
            try_mls_decrypt(&bob_db, conv_id, &alice_ct).is_none(),
            "removed bob must not decrypt"
        );

        // Alice adds Dave. Carol applies the commit.
        let dave_kp = gen_key_package(&dave_db, "dave");
        let (add_dave_commit, dave_welcome) = add_member_to_group(&alice_db, conv_id, &dave_kp);
        join_via_welcome(&dave_db, &dave_welcome);
        apply_commit(&carol_db, conv_id, &add_dave_commit);

        // All three current members (Alice, Carol, Dave) can communicate.
        let dave_ct = try_mls_encrypt(&dave_db, conv_id, b"dave here").unwrap();
        assert_eq!(try_mls_decrypt(&alice_db, conv_id, &dave_ct).unwrap(), b"dave here");
        assert_eq!(try_mls_decrypt(&carol_db, conv_id, &dave_ct).unwrap(), b"dave here");

        // Bob still cannot decrypt.
        assert!(
            try_mls_decrypt(&bob_db, conv_id, &dave_ct).is_none(),
            "bob must still be locked out after new member joins"
        );
    }

    /// After multiple add/remove cycles the group epoch is consistent and
    /// only current members can decrypt.
    #[test]
    fn key_rotation_across_multiple_membership_changes() {
        let conv_id = "01JTEST00000000000000000MN";

        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();

        create_group(&alice_db, conv_id, "alice");

        // Add Bob (epoch 0→1).
        let bob_kp = gen_key_package(&bob_db, "bob");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Add Carol (epoch 1→2). Bob applies commit.
        let carol_kp = gen_key_package(&carol_db, "carol");
        let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
        join_via_welcome(&carol_db, &carol_welcome);
        apply_commit(&bob_db, conv_id, &add_carol_commit);

        // Remove Bob (epoch 2→3). Carol applies commit.
        let remove_bob_commit = remove_member(&alice_db, conv_id, "bob");
        apply_commit(&carol_db, conv_id, &remove_bob_commit);

        // Alice and Carol can communicate at epoch 3.
        let ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
        assert_eq!(
            try_mls_decrypt(&carol_db, conv_id, &ct).unwrap(),
            b"bob is gone"
        );

        // Bob (stuck at epoch 2) cannot decrypt epoch-3 messages.
        assert!(
            try_mls_decrypt(&bob_db, conv_id, &ct).is_none(),
            "removed Bob must not decrypt after key rotation"
        );
    }

    /// Simulates account deletion: when a user is removed from a group (as
    /// part of account deletion), the remaining members' keys should rotate
    /// so the deleted user cannot decrypt future messages.
    ///
    /// The `delete_account` command (auth.rs) enumerates all groups and DM
    /// channels the user belongs to and calls `remove_member_mls_inner` for
    /// each before deleting DB rows.  This test verifies the underlying MLS
    /// removal + epoch advance works across multiple groups.
    ///
    /// See: https://github.com/actuallydan/pollis/issues/103
    #[test]
    fn account_deletion_rotates_keys_for_remaining_members() {
        let group1 = "01JTEST000000000000ACCTDEL1";
        let group2 = "01JTEST000000000000ACCTDEL2";

        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();

        // --- Group 1: Alice + Bob + Carol ---
        create_group(&alice_db, group1, "alice");

        let bob_kp1 = gen_key_package(&bob_db, "bob");
        let (_, bob_welcome1) = add_member_to_group(&alice_db, group1, &bob_kp1);
        join_via_welcome(&bob_db, &bob_welcome1);

        let carol_kp1 = gen_key_package(&carol_db, "carol");
        let (add_carol_commit1, carol_welcome1) = add_member_to_group(&alice_db, group1, &carol_kp1);
        join_via_welcome(&carol_db, &carol_welcome1);
        apply_commit(&bob_db, group1, &add_carol_commit1);

        // --- Group 2: Alice + Bob (Carol not in this one) ---
        create_group(&alice_db, group2, "alice");

        let bob_kp2 = gen_key_package(&bob_db, "bob");
        let (_, bob_welcome2) = add_member_to_group(&alice_db, group2, &bob_kp2);
        join_via_welcome(&bob_db, &bob_welcome2);

        // Verify Bob can read in both groups before deletion.
        let pre_g1 = try_mls_encrypt(&alice_db, group1, b"pre-delete g1").unwrap();
        assert_eq!(try_mls_decrypt(&bob_db, group1, &pre_g1).unwrap(), b"pre-delete g1");

        let pre_g2 = try_mls_encrypt(&alice_db, group2, b"pre-delete g2").unwrap();
        assert_eq!(try_mls_decrypt(&bob_db, group2, &pre_g2).unwrap(), b"pre-delete g2");

        // --- Simulate account deletion for Bob ---
        // In production this would be done by delete_account iterating all
        // groups and calling remove_member_mls_inner for each. Here we do
        // the MLS removal manually per group.

        // Remove Bob from group 1 — Carol applies commit.
        let remove_g1 = remove_member(&alice_db, group1, "bob");
        apply_commit(&carol_db, group1, &remove_g1);

        // Remove Bob from group 2 — no other non-alice members to notify.
        let _remove_g2 = remove_member(&alice_db, group2, "bob");

        // --- Verify key rotation: Bob locked out of both groups ---
        let post_g1 = try_mls_encrypt(&alice_db, group1, b"post-delete g1").unwrap();
        assert!(
            try_mls_decrypt(&bob_db, group1, &post_g1).is_none(),
            "deleted Bob must not decrypt group1 messages after account deletion"
        );

        let post_g2 = try_mls_encrypt(&alice_db, group2, b"post-delete g2").unwrap();
        assert!(
            try_mls_decrypt(&bob_db, group2, &post_g2).is_none(),
            "deleted Bob must not decrypt group2 messages after account deletion"
        );

        // --- Verify remaining members still work ---
        // Group 1: Alice and Carol can still communicate.
        assert_eq!(
            try_mls_decrypt(&carol_db, group1, &post_g1).unwrap(),
            b"post-delete g1"
        );
        let carol_msg = try_mls_encrypt(&carol_db, group1, b"carol still here").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_db, group1, &carol_msg).unwrap(),
            b"carol still here"
        );
    }
}
