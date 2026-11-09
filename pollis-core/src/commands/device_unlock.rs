//! Biometric unlock on mobile (#1685): Face ID / Touch ID / a Class 3 fingerprint can stand in
//! for TYPING the PIN at unlock, and for nothing else.
//!
//! While unlocked, and with the PIN just proven, core makes a random 32-byte device unlock key
//! (DUK) and writes the same `db_key` and `account_id_key` the PIN slots hold, wrapped under it.
//! The app keeps the DUK in the platform's biometric-bound store (iOS keychain
//! `.biometryCurrentSet` in an app-only access group; Android Keystore, Class 3 with a
//! `CryptoObject` per use) and hands it back here at the next unlock.
//!
//! Rules, each held by a test below or in the flows suite:
//! - Every user still has a PIN: no PIN slot, no biometric unlock ([`unlock_with_device_key`]
//!   goes through the PIN gate first, and `get_unlock_state` reports it only with `pin_set`).
//! - A biometric unlock never reads or writes the PIN's attempt counter, so a failed one costs
//!   nothing and a successful one forgives nothing. At the 10th wrong PIN the lockout nuke takes
//!   these slots with the PIN's.
//! - Turning it on needs the PIN: typed for it, or typed to unlock within
//!   [`crate::commands::pin::PIN_PROOF_WINDOW`] (the one-time offer). A biometric unlock never
//!   leaves that proof, so biometrics cannot re-arm biometrics.
//! - Device linking, history approval and PIN changes keep calling `verify_pin`; nothing here
//!   is accepted in its place.
//! - A PIN change rewraps nothing here: the DUK wraps the keys, not the PIN's KEK.
//!
//! Slots: `duk_meta` (version, `confirmed`, a verifier sealed under the DUK), `db_key_duk`,
//! `account_id_key_duk`. `device_unlock_begin` writes them unconfirmed and reads them back;
//! `device_unlock_confirm` marks them confirmed once the app has stored the DUK (#951's lesson: a
//! keystore `Ok(())` is not evidence). Unconfirmed slots never unlock anything.

use std::sync::Arc;

use base64::Engine as _;
use rand::RngCore;
use zeroize::Zeroizing;

use crate::commands::pin::{self, UnlockOutcome, UnlockState, KEK_LEN};
use crate::error::{Error, Result};
use crate::state::AppState;

const DUK_META_SLOT: &str = "duk_meta";
const DB_KEY_DUK_SLOT: &str = "db_key_duk";
const ACCOUNT_ID_KEY_DUK_SLOT: &str = "account_id_key_duk";

/// Every biometric slot. `auth::WIPED_USER_SLOTS` must hold each (tested).
pub(crate) const SLOTS: [&str; 3] = [DUK_META_SLOT, DB_KEY_DUK_SLOT, ACCOUNT_ID_KEY_DUK_SLOT];

const DUK_META_VERSION: u8 = 1;
const VERIFIER_PLAINTEXT: &[u8; 16] = b"pollis-duk-ok\0\0\0";

// duk_meta: version (1) | confirmed (1) | verifier (a `pin::wrap_bytes` blob under the DUK).
struct DukMeta {
    confirmed: bool,
    verifier: Vec<u8>,
}

impl DukMeta {
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.verifier.len());
        out.push(DUK_META_VERSION);
        out.push(u8::from(self.confirmed));
        out.extend_from_slice(&self.verifier);
        out
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2 {
            return Err(Error::Crypto("duk_meta: short blob".into()));
        }
        if bytes[0] != DUK_META_VERSION {
            return Err(Error::Crypto(format!("duk_meta: unknown version {}", bytes[0])));
        }
        let confirmed = match bytes[1] {
            0 => false,
            1 => true,
            other => return Err(Error::Crypto(format!("duk_meta: bad flag {other}"))),
        };
        Ok(Self {
            confirmed,
            verifier: bytes[2..].to_vec(),
        })
    }
}

type Duk = Zeroizing<[u8; KEK_LEN]>;

fn decode_duk(device_key: &str) -> Result<Duk> {
    let raw = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(device_key.trim())
            .map_err(|_| Error::Crypto("device unlock key is not valid base64".into()))?,
    );
    if raw.len() != KEK_LEN {
        return Err(Error::Crypto("device unlock key has the wrong length".into()));
    }
    let mut duk = Zeroizing::new([0u8; KEK_LEN]);
    duk.copy_from_slice(&raw);
    Ok(duk)
}

fn encode_duk(duk: &Duk) -> String {
    base64::engine::general_purpose::STANDARD.encode(&**duk)
}

async fn load_meta(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
) -> Result<Option<DukMeta>> {
    match keystore.load_for_user(DUK_META_SLOT, user_id).await? {
        None => Ok(None),
        Some(bytes) => Ok(Some(DukMeta::from_bytes(&bytes)?)),
    }
}

fn verifier_matches(duk: &Duk, meta: &DukMeta) -> bool {
    pin::unwrap_bytes(duk, &meta.verifier)
        .map(|pt| pt.as_slice() == VERIFIER_PLAINTEXT.as_slice())
        .unwrap_or(false)
}

/// Unwrap both keys under `duk`, after the verifier. Reads only the biometric slots.
async fn unwrap_keys(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    duk: &Duk,
    meta: &DukMeta,
) -> Result<(Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>)> {
    if !verifier_matches(duk, meta) {
        return Err(Error::Crypto("device unlock key does not match".into()));
    }
    let load = |slot: &'static str| async move {
        keystore
            .load_for_user(slot, user_id)
            .await?
            .ok_or_else(|| Error::Crypto(format!("{slot} missing")))
    };
    let db_key = pin::unwrap_bytes(duk, &load(DB_KEY_DUK_SLOT).await?)?;
    let account_id_key = pin::unwrap_bytes(duk, &load(ACCOUNT_ID_KEY_DUK_SLOT).await?)?;
    Ok((db_key, account_id_key))
}

/// Delete every biometric slot for `user_id`. Best-effort, like the other wipe paths.
pub(crate) async fn delete_slots(keystore: &dyn crate::keystore::Keystore, user_id: &str) {
    for slot in SLOTS {
        let _ = keystore.delete_for_user(slot, user_id).await;
    }
}

/// Whether `user_id` has confirmed biometric slots. A read error answers false.
pub(crate) async fn is_enabled(keystore: &dyn crate::keystore::Keystore, user_id: &str) -> bool {
    matches!(load_meta(keystore, user_id).await, Ok(Some(meta)) if meta.confirmed)
}

/// The active user and their unlocked keys; biometric setup only runs while unlocked.
async fn unlocked_keys(
    state: &Arc<AppState>,
) -> Result<(String, Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>)> {
    let user_id = crate::accounts::read_accounts_index()
        .unwrap_or_default()
        .last_active_user
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no active user")))?;
    let guard = state.unlock.lock().await;
    match guard.as_ref() {
        Some(u) if u.user_id == user_id => {
            Ok((user_id, u.db_key.clone(), u.account_id_key.clone()))
        }
        _ => Err(Error::Other(anyhow::anyhow!("unlock Pollis first"))),
    }
}

/// Write the biometric slots for `user_id` under `duk`, unconfirmed, and read them back.
///
/// `duk_meta` goes first: from that write on, an older DUK fails the verifier, so an interrupted
/// begin can never pair an old key with new blobs. Exposed to the tests as the seam a dropping
/// keystore is driven through.
async fn write_slots(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    duk: &Duk,
    db_key: &[u8],
    account_id_key: &[u8],
) -> Result<()> {
    let meta = DukMeta {
        confirmed: false,
        verifier: pin::wrap_bytes(duk, VERIFIER_PLAINTEXT)?,
    };
    keystore
        .store_for_user(DUK_META_SLOT, user_id, &meta.to_bytes())
        .await?;
    keystore
        .store_for_user(DB_KEY_DUK_SLOT, user_id, &pin::wrap_bytes(duk, db_key)?)
        .await?;
    keystore
        .store_for_user(
            ACCOUNT_ID_KEY_DUK_SLOT,
            user_id,
            &pin::wrap_bytes(duk, account_id_key)?,
        )
        .await?;
    check_slots(keystore, user_id, duk, db_key, account_id_key)
        .await
        .map(|_| ())
}

/// Prove the slots on disk unwrap under `duk` to exactly these keys.
async fn check_slots(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    duk: &Duk,
    db_key: &[u8],
    account_id_key: &[u8],
) -> Result<DukMeta> {
    let fail = || {
        Error::Crypto(
            "biometric unlock could not be turned on: the device keystore did not return what \
             was written. Try again."
                .into(),
        )
    };
    let meta = load_meta(keystore, user_id).await?.ok_or_else(fail)?;
    let (got_db, got_acct) = unwrap_keys(keystore, user_id, duk, &meta)
        .await
        .map_err(|_| fail())?;
    if *got_db != *db_key || *got_acct != *account_id_key {
        return Err(fail());
    }
    Ok(meta)
}

/// Start turning biometric unlock on: new DUK, unconfirmed slots, the DUK returned (base64) for
/// the app to put in the biometric store. `pin` is required unless the PIN was typed to unlock
/// within the proof window; a typed PIN counts toward the lockout like any other.
pub async fn device_unlock_begin(state: &Arc<AppState>, pin: Option<String>) -> Result<String> {
    let (user_id, db_key, account_id_key) = unlocked_keys(state).await?;
    let keystore = state.keystore.as_ref();
    pin::require_pin_not_locked_out(keystore, &user_id).await?;
    match pin {
        Some(p) => pin::verify_pin(state, &user_id, &p).await?,
        None => {
            if !pin::take_pin_proof(state, &user_id).await {
                return Err(Error::Other(anyhow::anyhow!(
                    "Enter your PIN to turn on biometric unlock."
                )));
            }
        }
    }

    let mut duk: Duk = Zeroizing::new([0u8; KEK_LEN]);
    rand::rngs::OsRng.fill_bytes(&mut *duk);
    write_slots(keystore, &user_id, &duk, &db_key, &account_id_key).await?;
    Ok(encode_duk(&duk))
}

/// Finish turning it on: the app stored `device_key` (and, where it can without a second prompt,
/// read it back). Checks it against the slots and the unlocked keys, then marks them confirmed
/// and reads the flag back.
pub async fn device_unlock_confirm(state: &Arc<AppState>, device_key: String) -> Result<()> {
    let (user_id, db_key, account_id_key) = unlocked_keys(state).await?;
    let keystore = state.keystore.as_ref();
    let duk = decode_duk(&device_key)?;
    let mut meta = check_slots(keystore, &user_id, &duk, &db_key, &account_id_key).await?;
    meta.confirmed = true;
    keystore
        .store_for_user(DUK_META_SLOT, &user_id, &meta.to_bytes())
        .await?;
    if !is_enabled(keystore, &user_id).await {
        return Err(Error::Crypto(
            "biometric unlock could not be turned on: the device keystore did not keep it. \
             Try again."
                .into(),
        ));
    }
    Ok(())
}

/// Unlock with the DUK the app read from the biometric store, then the PIN's own
/// [`pin::finish_unlock`]. Never touches the PIN's attempt counter.
pub async fn unlock_with_device_key(
    state: &Arc<AppState>,
    user_id: String,
    device_key: String,
) -> Result<UnlockOutcome> {
    let keystore = state.keystore.as_ref();
    pin::require_pin_not_locked_out(keystore, &user_id).await?;
    let meta = load_meta(keystore, &user_id)
        .await?
        .filter(|m| m.confirmed)
        .ok_or_else(|| Error::Other(anyhow::anyhow!("biometric unlock is off")))?;
    let duk = decode_duk(&device_key)?;
    let (db_key, account_id_key) = unwrap_keys(keystore, &user_id, &duk, &meta).await?;
    pin::finish_unlock(
        state,
        UnlockState {
            user_id,
            db_key,
            account_id_key,
        },
    )
    .await
}

/// Turn biometric unlock off for the active user. Needs no PIN: it only takes a way in away.
pub async fn disable_device_unlock(state: &Arc<AppState>) -> Result<()> {
    let user_id = crate::accounts::read_accounts_index()
        .unwrap_or_default()
        .last_active_user
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no active user")))?;
    delete_slots(state.keystore.as_ref(), &user_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::{InMemoryKeystore, Keystore};

    const DB: [u8; 32] = [7u8; 32];
    const ACCT: [u8; 32] = [42u8; 32];

    fn fresh_duk() -> Duk {
        let mut duk: Duk = Zeroizing::new([0u8; KEK_LEN]);
        rand::rngs::OsRng.fill_bytes(&mut *duk);
        duk
    }

    async fn confirm(ks: &dyn Keystore, uid: &str) {
        let mut meta = load_meta(ks, uid).await.unwrap().unwrap();
        meta.confirmed = true;
        ks.store_for_user(DUK_META_SLOT, uid, &meta.to_bytes())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_device_key_unwraps_the_same_keys() {
        let ks = InMemoryKeystore::new();
        let duk = fresh_duk();
        write_slots(&ks, "u1", &duk, &DB, &ACCT).await.unwrap();
        let meta = load_meta(&ks, "u1").await.unwrap().unwrap();
        let (db, acct) = unwrap_keys(&ks, "u1", &duk, &meta).await.unwrap();
        assert_eq!(&*db, &DB);
        assert_eq!(&*acct, &ACCT);
    }

    #[tokio::test]
    async fn written_slots_start_unconfirmed() {
        let ks = InMemoryKeystore::new();
        write_slots(&ks, "u1", &fresh_duk(), &DB, &ACCT).await.unwrap();
        assert!(!is_enabled(&ks, "u1").await);
        confirm(&ks, "u1").await;
        assert!(is_enabled(&ks, "u1").await);
    }

    #[tokio::test]
    async fn a_wrong_or_malformed_key_never_unwraps() {
        let ks = InMemoryKeystore::new();
        write_slots(&ks, "u1", &fresh_duk(), &DB, &ACCT).await.unwrap();
        let meta = load_meta(&ks, "u1").await.unwrap().unwrap();
        assert!(unwrap_keys(&ks, "u1", &fresh_duk(), &meta).await.is_err());
        assert!(decode_duk("not base64!").is_err());
        assert!(decode_duk(&base64::engine::general_purpose::STANDARD.encode([1u8; 16])).is_err());
    }

    #[tokio::test]
    async fn one_users_key_never_unlocks_another() {
        let ks = InMemoryKeystore::new();
        let a = fresh_duk();
        let b = fresh_duk();
        write_slots(&ks, "alice", &a, &DB, &ACCT).await.unwrap();
        write_slots(&ks, "bob", &b, &[1u8; 32], &[2u8; 32]).await.unwrap();
        let bob_meta = load_meta(&ks, "bob").await.unwrap().unwrap();
        assert!(unwrap_keys(&ks, "bob", &a, &bob_meta).await.is_err());
        // And the slots are per user: Alice's survive Bob's being deleted.
        delete_slots(&ks, "bob").await;
        assert!(load_meta(&ks, "alice").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_new_begin_invalidates_the_old_key_from_its_first_write() {
        let ks = InMemoryKeystore::new();
        let old = fresh_duk();
        write_slots(&ks, "u1", &old, &DB, &ACCT).await.unwrap();
        confirm(&ks, "u1").await;
        // A second begin interrupted right after its meta write: only meta is new.
        let new = fresh_duk();
        let meta = DukMeta {
            confirmed: false,
            verifier: pin::wrap_bytes(&new, VERIFIER_PLAINTEXT).unwrap(),
        };
        ks.store_for_user(DUK_META_SLOT, "u1", &meta.to_bytes())
            .await
            .unwrap();
        assert!(!is_enabled(&ks, "u1").await);
        assert!(unwrap_keys(&ks, "u1", &old, &meta).await.is_err());
    }

    /// A keystore that reports a write for one slot but does not keep it (#184 / #951).
    struct DropsWritesTo {
        inner: InMemoryKeystore,
        slot_prefix: &'static str,
        silently: bool,
    }

    #[async_trait::async_trait]
    impl Keystore for DropsWritesTo {
        async fn store(&self, key: &str, value: &[u8]) -> Result<()> {
            if key.starts_with(self.slot_prefix) {
                return if self.silently {
                    Ok(())
                } else {
                    Err(Error::Crypto("keystore write refused (test)".into()))
                };
            }
            self.inner.store(key, value).await
        }
        async fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
            self.inner.load(key).await
        }
        async fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key).await
        }
    }

    #[tokio::test]
    async fn dropped_writes_leave_the_slots_unconfirmed() {
        for silently in [true, false] {
            for slot_prefix in SLOTS {
                let ks = DropsWritesTo {
                    inner: InMemoryKeystore::new(),
                    slot_prefix,
                    silently,
                };
                let err = write_slots(&ks, "u1", &fresh_duk(), &DB, &ACCT)
                    .await
                    .expect_err("a write that did not land must fail the begin");
                assert!(
                    !is_enabled(&ks, "u1").await,
                    "{slot_prefix} (silently={silently}, {err}): nothing may be confirmed"
                );
            }
        }
    }

    #[test]
    fn every_wipe_path_list_holds_the_slots() {
        for slot in SLOTS {
            assert!(
                crate::commands::auth::WIPED_USER_SLOTS.contains(&slot),
                "{slot} must be in WIPED_USER_SLOTS"
            );
        }
    }

    #[test]
    fn duk_meta_roundtrips_and_rejects_junk() {
        let m = DukMeta {
            confirmed: true,
            verifier: vec![9u8; 10],
        };
        let back = DukMeta::from_bytes(&m.to_bytes()).unwrap();
        assert!(back.confirmed);
        assert_eq!(back.verifier, vec![9u8; 10]);
        assert!(DukMeta::from_bytes(&[2, 1]).is_err());
        assert!(DukMeta::from_bytes(&[1, 7]).is_err());
        assert!(DukMeta::from_bytes(&[1]).is_err());
    }
}
