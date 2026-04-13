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

use openmls::prelude::group_info::VerifiableGroupInfo;
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

    /// Borrow the raw sqlite connection backing `mls_kv`. Used for custom
    /// rows Pollis writes alongside openmls state (e.g. the stable per-
    /// device signing key reference).
    pub fn raw_conn(&self) -> &rusqlite::Connection {
        self.store.raw_conn()
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
pub fn make_credential(user_id: &str, device_id: &str) -> Credential {
    BasicCredential::new(format!("{user_id}:{device_id}").into_bytes()).into()
}

/// Extract the `user_id` from a credential produced by `make_credential`.
///
/// Handles legacy credentials that contain only `user_id` (no colon).
pub fn parse_credential_user_id(cred: &Credential) -> String {
    let s = String::from_utf8_lossy(cred.serialized_content());
    s.split_once(':').map(|(u, _)| u).unwrap_or(&s).to_string()
}

/// Extract the `device_id` from a credential produced by `make_credential`.
///
/// Returns `None` for legacy credentials that contain only `user_id`.
pub fn parse_credential_device_id(cred: &Credential) -> Option<String> {
    let s = String::from_utf8_lossy(cred.serialized_content()).into_owned();
    s.split_once(':').map(|(_, d)| d.to_string())
}

// ── Per-device stable MLS signing key ────────────────────────────────────────

/// Custom scope in `mls_kv` that stores the stable per-device MLS
/// signature public-key bytes. The private side is held by openmls under
/// its own `SignatureKeyPair` scope, looked up by these same bytes.
const DEVICE_SIG_PUB_SCOPE: &str = "PollisDeviceSigPub";

fn load_stable_device_sig_pub_bytes(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<Option<Vec<u8>>> {
    let key = format!("{user_id}:{device_id}").into_bytes();
    let mut stmt = conn.prepare(
        "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
    )?;
    use rusqlite::OptionalExtension;
    let row: Option<Vec<u8>> = stmt
        .query_row(rusqlite::params![DEVICE_SIG_PUB_SCOPE, key], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .optional()?;
    Ok(row)
}

fn store_stable_device_sig_pub_bytes(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
    pub_bytes: &[u8],
) -> crate::error::Result<()> {
    let key = format!("{user_id}:{device_id}").into_bytes();
    conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![DEVICE_SIG_PUB_SCOPE, key, pub_bytes],
    )?;
    Ok(())
}

/// Return the stable MLS signing keypair for this device, creating it if
/// missing. All key packages and group creation on this device MUST use
/// this keypair so the device-level cross-signing cert in `user_device`
/// covers every leaf node this device produces.
///
/// Returns `(SignatureKeyPair, pub_bytes)`. The pub_bytes are also what
/// gets signed into the `device_cert` in `user_device`.
pub fn load_or_create_device_signer(
    provider: &PollisProvider<'_>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<(SignatureKeyPair, Vec<u8>)> {
    // Fast path: pub bytes are stashed → recover the private side from
    // openmls storage and return.
    if let Some(pub_bytes) = load_stable_device_sig_pub_bytes(
        provider.raw_conn(),
        user_id,
        device_id,
    )? {
        if let Some(kp) = SignatureKeyPair::read(
            provider.storage(),
            &pub_bytes,
            CS.signature_algorithm(),
        ) {
            return Ok((kp, pub_bytes));
        }
        // Pub bytes stashed but the private side is gone (e.g. mls_kv
        // got partially wiped). Fall through to regenerate.
        eprintln!(
            "[mls] stable device signer pub present but private missing for {user_id}:{device_id} — regenerating"
        );
    }

    // Slow path: create, store, stash.
    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;
    sig_keys
        .store(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;
    let pub_bytes = sig_keys.to_public_vec();
    store_stable_device_sig_pub_bytes(provider.raw_conn(), user_id, device_id, &pub_bytes)?;
    Ok((sig_keys, pub_bytes))
}

// ── Device cross-signing ─────────────────────────────────────────────────────

/// Ensure this device has a stable MLS signing keypair AND a `device_cert`
/// published in `user_device` binding the pub bytes to the user's
/// `account_id_key`. Idempotent — safe to call on every login.
///
/// Skipped if `account_id_key` is not in the local OS keystore (i.e. this
/// is a returning user on a device that has never been enrolled yet).
/// Returns `true` if a cert was written, `false` if skipped.
pub async fn ensure_device_cert(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<bool> {
    // 0. Bail early if we don't have the account identity locally. This
    //    happens on a new device before step-5 enrollment has run.
    if !crate::commands::account_identity::has_local_account_identity(user_id).await? {
        return Ok(false);
    }

    // 1. Load or create the stable per-device MLS signing keypair and
    //    capture its public bytes. Sync openmls work inside a scope.
    let sig_pub_bytes = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        let (_sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, user_id, device_id)?;
        sig_pub_bytes
    };

    // 2. Read the current identity_version for this user from the remote
    //    `users` table. Defaults to 1 if the column is NULL (shouldn't
    //    happen post-migration-13 but is defensive).
    let conn = state.remote_db.conn().await?;
    let identity_version: u32 = {
        let mut rows = conn
            .query(
                "SELECT identity_version FROM users WHERE id = ?1",
                libsql::params![user_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<i64>(0).unwrap_or(1) as u32,
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "user {user_id} not found while signing device cert"
                )))
            }
        }
    };

    // 3. Sign the cert with the account identity key loaded from the OS
    //    keystore, using the current unix time as `issued_at`.
    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cert = crate::commands::account_identity::sign_device_cert(
        user_id,
        device_id,
        &sig_pub_bytes,
        identity_version,
        issued_at,
    )
    .await?;

    // 4. Write cert + signing pub + issued_at + identity_version into
    //    the remote `user_device` row. Other clients read these columns
    //    before accepting this device into any MLS group.
    //
    // `cert_issued_at` is stored as a decimal string of unix seconds —
    // the migration created the column as TEXT, and we need lossless
    // round-trip to u64 for signature verification later.
    let issued_at_str = issued_at.to_string();

    conn.execute(
        "UPDATE user_device \
         SET device_cert = ?1, \
             cert_issued_at = ?2, \
             cert_identity_version = ?3, \
             mls_signature_pub = ?4 \
         WHERE device_id = ?5",
        libsql::params![
            cert,
            issued_at_str,
            identity_version as i64,
            sig_pub_bytes,
            device_id
        ],
    )
    .await?;

    eprintln!(
        "[mls] device cert published for {user_id}:{device_id} (identity_version={identity_version})"
    );

    Ok(true)
}

// ── GroupInfo publishing ─────────────────────────────────────────────────────

/// Export a fresh `GroupInfo` for the given conversation and upsert it
/// into the remote `mls_group_info` table. Called by every device that
/// merges a commit (the originator right after `merge_pending_commit`,
/// receivers right after `merge_staged_commit`).
///
/// The row is conversation-scoped and only overwritten with a STRICTLY
/// greater epoch, so concurrent writers at the same epoch are idempotent
/// and receivers don't waste work once the committer has already
/// published.
///
/// No-op if:
///   - the device has no local MLS group for this conversation
///   - the device has no `account_id_key` (pre-enrollment)
///
/// This function is the prerequisite for the Secret Key recovery path:
/// a brand-new device uses the stored `GroupInfo` to construct an MLS
/// external commit joining the group, without needing a Welcome.
pub async fn publish_group_info(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> crate::error::Result<()> {
    // Sync scope: load the local group, recover the signer, export a
    // GroupInfo, and TLS-serialize it. Nothing !Send crosses await.
    let device_id_opt = state.device_id.lock().await.clone();
    let Some(device_id) = device_id_opt else {
        return Ok(());
    };

    let exported: Option<(u64, Vec<u8>)> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(());
        };
        let provider = PollisProvider::new(db.conn());
        let (group, signer) = match load_group_with_signer(&provider, conversation_id) {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let epoch = group.epoch().as_u64();
        let msg = match group.export_group_info(provider.crypto(), &signer, true) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[mls] publish_group_info: export failed for {conversation_id}: {e}");
                return Ok(());
            }
        };
        let bytes = msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("group_info serialize: {e}")))?;
        Some((epoch, bytes))
    };

    let Some((epoch, bytes)) = exported else {
        return Ok(());
    };

    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO mls_group_info \
         (conversation_id, epoch, group_info, updated_at, updated_by_device_id) \
         VALUES (?1, ?2, ?3, datetime('now'), ?4) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
             epoch = excluded.epoch, \
             group_info = excluded.group_info, \
             updated_at = datetime('now'), \
             updated_by_device_id = excluded.updated_by_device_id \
         WHERE excluded.epoch > mls_group_info.epoch",
        libsql::params![conversation_id, epoch as i64, bytes, device_id],
    )
    .await?;

    Ok(())
}

// ── Inbound cert verification helper ────────────────────────────────────────

/// Verify that every `device_id` in `device_ids` has a valid
/// cross-signing cert that chains to the `account_id_pub` of
/// `target_user_id`. Returns `Ok(true)` if all devices check out,
/// `Ok(false)` if any single device fails, `Err` on a database
/// lookup error.
///
/// Called from `process_pending_commits_inner` against the metadata
/// columns on `mls_commit_log` BEFORE handing the commit to
/// `process_message`. This is the inbound complement to the outbound
/// cert verification in `reconcile_group_mls_impl`.
async fn verify_added_devices(
    conn: &libsql::Connection,
    target_user_id: &str,
    device_ids: &[String],
) -> crate::error::Result<bool> {
    if device_ids.is_empty() {
        return Ok(true);
    }

    // Fetch account_id_pub once.
    let account_id_pub: Vec<u8> = {
        let mut rows = conn
            .query(
                "SELECT account_id_pub FROM users WHERE id = ?1",
                libsql::params![target_user_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => match row.get::<Option<Vec<u8>>>(0).ok().flatten() {
                Some(b) => b,
                None => {
                    eprintln!(
                        "[mls] verify_added_devices: {target_user_id} has no account_id_pub"
                    );
                    return Ok(false);
                }
            },
            None => {
                eprintln!(
                    "[mls] verify_added_devices: user {target_user_id} not found"
                );
                return Ok(false);
            }
        }
    };

    for did in device_ids {
        let mut rows = conn
            .query(
                "SELECT device_cert, cert_issued_at, cert_identity_version, mls_signature_pub \
                 FROM user_device WHERE device_id = ?1 AND user_id = ?2",
                libsql::params![did.as_str(), target_user_id],
            )
            .await?;

        let row = match rows.next().await? {
            Some(r) => r,
            None => {
                eprintln!(
                    "[mls] verify_added_devices: device {did} not registered for {target_user_id}"
                );
                return Ok(false);
            }
        };

        let cert: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(0).ok().flatten();
        let issued_at_str: Option<String> = row.get::<Option<String>>(1).ok().flatten();
        let cert_identity_version: Option<i64> = row.get::<Option<i64>>(2).ok().flatten();
        let mls_sig_pub: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(3).ok().flatten();
        drop(rows);

        let (cert, issued_at_str, cert_identity_version, mls_sig_pub) =
            match (cert, issued_at_str, cert_identity_version, mls_sig_pub) {
                (Some(c), Some(t), Some(v), Some(p)) => (c, t, v, p),
                _ => {
                    eprintln!(
                        "[mls] verify_added_devices: device {did} has no cert columns populated"
                    );
                    return Ok(false);
                }
            };

        let issued_at: u64 = match issued_at_str.parse() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[mls] verify_added_devices: device {did} cert_issued_at unparseable '{issued_at_str}': {e}"
                );
                return Ok(false);
            }
        };

        if let Err(e) = crate::commands::account_identity::verify_device_cert(
            &account_id_pub,
            did,
            &mls_sig_pub,
            cert_identity_version as u32,
            issued_at,
            &cert,
        ) {
            eprintln!(
                "[mls] verify_added_devices: device {did} cert verification failed: {e}"
            );
            return Ok(false);
        }
    }

    Ok(true)
}

// ── External-commit joining ──────────────────────────────────────────────────

/// Join an existing MLS group via external commit, using the latest
/// `GroupInfo` blob stored server-side in `mls_group_info`. The new
/// device becomes a full member of the group at the epoch *after* the
/// one carried in the GroupInfo.
///
/// Used by the Secret Key recovery path: when a new device recovers
/// `account_id_key` without any sibling device online to issue a
/// Welcome, it fetches each of the user's groups' GroupInfo and
/// externally commits into them. The commit is posted to
/// `mls_commit_log` so existing members will merge it on their next
/// `process_pending_commits` pass.
///
/// Safety note: this path does NOT currently pass through the outbound
/// cross-signing cert check. Existing members that implement the
/// step-3b inbound cert verification will reject external-join commits
/// from devices whose cert doesn't chain to the user's
/// `account_id_pub` — which is exactly the desired behavior.
pub async fn external_join_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;

    // 1. Fetch the stored GroupInfo for this conversation.
    let (group_info_bytes, stored_epoch): (Vec<u8>, i64) = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn
            .query(
                "SELECT group_info, epoch FROM mls_group_info WHERE conversation_id = ?1",
                libsql::params![conversation_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => (row.get(0)?, row.get(1)?),
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "no GroupInfo stored for {conversation_id} — cannot external-join"
                )))
            }
        }
    };

    // 2. Run the external commit inside the local_db sync scope.
    let commit_bytes: Vec<u8> = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());

        let mut env_reader: &[u8] = &group_info_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut env_reader).map_err(|e| {
            crate::error::Error::Other(anyhow::anyhow!(
                "stored group_info envelope failed to deserialize: {e}"
            ))
        })?;
        let verifiable_group_info = match msg_in.extract() {
            MlsMessageBodyIn::GroupInfo(gi) => gi,
            other => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "expected GroupInfo in mls_group_info, got {:?}",
                    std::mem::discriminant(&other)
                )));
            }
        };

        // Load (or create) this device's stable MLS signing keypair.
        let (sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, user_id, &device_id)?;

        let credential = make_credential(user_id, &device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_pub_bytes.into(),
            CS.signature_algorithm(),
        )
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        // Drop any stale local group with the same ID so the external
        // commit builder doesn't collide.
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
            let _ = old.delete(provider.storage());
        }

        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        let (_joined_group, commit_bundle) = MlsGroup::external_commit_builder()
            .with_config(join_config)
            .build_group(&provider, verifiable_group_info, cred_with_key)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit build_group: {e}"
            )))?
            .load_psks(provider.storage())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit load_psks: {e}"
            )))?
            .build(provider.rand(), provider.crypto(), &sig_keys, |_| true)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit build: {e}"
            )))?
            .finalize(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit finalize: {e}"
            )))?;

        let (commit_msg, _welcome_msg, _new_group_info) = commit_bundle.into_contents();
        commit_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("commit serialize: {e}")))?
    };

    // 3. Post the commit to mls_commit_log so existing members will
    //    process it on their next process_pending_commits pass.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO mls_commit_log \
         (conversation_id, epoch, sender_id, commit_data, added_user_id, added_device_ids) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![
            conversation_id,
            stored_epoch,
            user_id,
            commit_bytes,
            user_id,
            device_id.clone()
        ],
    )
    .await?;

    // 4. Refresh the stored GroupInfo at the new epoch so any NEXT
    //    new device joining via this same path sees the up-to-date
    //    tree.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        eprintln!(
            "[mls] external_join_group: publish_group_info failed (non-fatal): {e}"
        );
    }

    eprintln!(
        "[mls] external_join_group: {user_id}:{device_id} joined {conversation_id} from epoch {stored_epoch}"
    );

    Ok(())
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

        let (sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, &user_id, &device_id)?;

        let credential = make_credential(&user_id, &device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_pub_bytes.into(),
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

    // Generate and publish TARGET fresh packages. They all share the same
    // stable device signing key so one `device_cert` in `user_device`
    // covers every key package this device ever ships.
    for _ in 0..TARGET {
        let (ref_hex, kp_bytes) = {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
            })?;
            let provider = PollisProvider::new(db.conn());

            let (sig_keys, sig_pub_bytes) =
                load_or_create_device_signer(&provider, user_id, device_id)?;

            let credential = make_credential(user_id, device_id);
            let sig_pub = OpenMlsSignaturePublicKey::new(
                sig_pub_bytes.into(),
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

            let (sig_keys, sig_pub_bytes) =
                load_or_create_device_signer(&provider, user_id, device_id)?;

            let credential = make_credential(user_id, device_id);
            let sig_pub = OpenMlsSignaturePublicKey::new(
                sig_pub_bytes.into(),
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

    // Scope the local_db guard so it is dropped before the async
    // publish_group_info call below (which re-acquires it).
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());

        let (sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, creator_user_id, &device_id)?;

        let credential = make_credential(creator_user_id, &device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_pub_bytes.into(),
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
    }

    // Publish the epoch-0 GroupInfo so a future device enrolling via the
    // Secret Key path can join this group via external commit.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        eprintln!("[mls] init_mls_group: publish_group_info failed (non-fatal): {e}");
    }

    Ok(())
}

/// Create a fresh MLS group for `conversation_id` (a channel or DM ULID).
/// The creator becomes the sole initial member.  Other users are added via
/// `reconcile_group_mls`.
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
            Ok(()) => {
                eprintln!("[mls] poll_mls_welcomes: applied welcome {id}");
            }
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

/// Apply any commits from `mls_commit_log` that this member has not yet seen.
///
/// Reads rows where `epoch >= current_local_epoch` in ascending order, applies
/// each commit, and advances the local epoch.  An epoch gap (unexpected jump)
/// stops processing and logs an error — this indicates a missed or reordered
/// commit that would require manual intervention in a production system.
///
/// `mls_group_id` must already be resolved (group_id for channels,
/// conversation_id for DMs).
/// Ensure this device has a local MLS group at the latest epoch for
/// `mls_group_id`. Processes any pending commits from the commit log.
/// If the local group is missing, evicted, or unrecoverably behind,
/// falls back to external-join using the published GroupInfo.
///
/// `user_id` is needed for the external-join fallback.
pub async fn process_pending_commits_inner(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    // 1. Get the current epoch from the local group.
    let has_group = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(mls_group_id.as_bytes());
        MlsGroup::load(provider.storage(), &group_id)
            .ok()
            .flatten()
            .map(|g| g.epoch().as_u64())
    };
    let initial_epoch = match has_group {
        Some(epoch) => epoch,
        None => {
            // No local group — external-join to create one.
            if let Err(e) = external_join_group(state, mls_group_id, user_id).await {
                eprintln!("[mls] process_pending_commits: no local group for {mls_group_id}, external-join failed: {e}");
            }
            return Ok(());
        }
    };

    // 2. Fetch pending commits from remote, along with the add-metadata
    //    columns (`added_user_id`, `added_device_ids`) so we can verify
    //    cross-signing certs BEFORE calling `process_message`. Collected
    //    into an owned Vec so the `rows` cursor is dropped before any
    //    local-DB await below.
    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT epoch, commit_data, added_user_id, added_device_ids \
         FROM mls_commit_log \
         WHERE conversation_id = ?1 AND epoch >= ?2 \
         ORDER BY epoch ASC, seq ASC",
        libsql::params![mls_group_id, initial_epoch as i64],
    ).await?;

    #[derive(Debug)]
    struct PendingCommit {
        epoch: i64,
        commit_data: Vec<u8>,
        added_user_id: Option<String>,
        added_device_ids: Vec<String>,
    }

    let mut pending: Vec<PendingCommit> = Vec::new();
    while let Some(row) = rows.next().await? {
        let epoch: i64 = row.get(0)?;
        let data: Vec<u8> = row.get(1)?;
        let added_user_id: Option<String> = row.get::<Option<String>>(2).ok().flatten();
        let ids_csv: Option<String> = row.get::<Option<String>>(3).ok().flatten();
        let added_device_ids: Vec<String> = ids_csv
            .as_deref()
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        pending.push(PendingCommit {
            epoch,
            commit_data: data,
            added_user_id,
            added_device_ids,
        });
    }
    drop(rows);

    // 3. Apply each commit in epoch order. For any commit carrying add
    //    metadata, verify every added device's cross-signing cert
    //    against the user's account_id_pub BEFORE touching the group
    //    state.
    let mut current_epoch = initial_epoch;
    let mut any_applied = false;
    'commit_loop: for commit in pending {
        if commit.epoch as u64 != current_epoch {
            eprintln!(
                "[mls] process_pending_commits: epoch gap for {mls_group_id}: \
                 expected {current_epoch}, got {} — stopping",
                commit.epoch
            );
            break;
        }

        // ── Inbound cert verification ──────────────────────────────
        // If this commit claims to add any devices, verify each of
        // their certs chains to the user's published account_id_pub.
        // A failed verification rejects the ENTIRE commit — we stop
        // processing rather than skip, because subsequent commits
        // target a group state we can no longer reach.
        if let Some(ref added_user_id) = commit.added_user_id {
            let ok = verify_added_devices(
                &conn,
                added_user_id,
                &commit.added_device_ids,
            )
            .await;
            match ok {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!(
                        "[mls] process_pending_commits: rejecting commit at epoch {} for {mls_group_id} — cross-signing verification failed for {added_user_id}",
                        commit.epoch
                    );
                    break 'commit_loop;
                }
                Err(e) => {
                    eprintln!(
                        "[mls] process_pending_commits: cert verification error for {mls_group_id}: {e} — stopping"
                    );
                    break 'commit_loop;
                }
            }
        }

        let commit_data = commit.commit_data;

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
                    eprintln!("[mls] process_pending_commits: deserialize failed for {mls_group_id} at epoch {}: {e} — stopping", commit.epoch);
                    break;
                }
            };
            let protocol_msg = match msg_in.try_into_protocol_message() {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[mls] process_pending_commits: protocol msg failed for {mls_group_id} at epoch {}: {e} — stopping", commit.epoch);
                    break;
                }
            };

            match group.process_message(&provider, protocol_msg) {
                Ok(processed) => {
                    if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
                        if let Err(e) = group.merge_staged_commit(&provider, *staged) {
                            eprintln!("[mls] process_pending_commits: merge failed for {mls_group_id} at epoch {}: {e} — stopping", commit.epoch);
                            break;
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{e}");
                    // If we were evicted (kicked), delete the stale group so
                    // external-join recovery can create a fresh one.
                    if msg.contains("evicted") {
                        eprintln!("[mls] process_pending_commits: evicted from {mls_group_id} — deleting local group for recovery");
                        let _ = group.delete(provider.storage());
                    } else {
                        eprintln!("[mls] process_pending_commits: {e} for {mls_group_id} at epoch {} — stopping", commit.epoch);
                    }
                    break;
                }
            }

            true
        };

        if applied {
            current_epoch += 1;
            any_applied = true;
        }
    }

    // If we merged at least one commit, refresh the stored GroupInfo. The
    // committer already published when they issued the commit, but
    // publishing again here keeps the row fresh if the committer's write
    // failed transiently. The UPSERT's `WHERE excluded.epoch >` guard
    // means we only overwrite if we have a strictly newer epoch, so
    // redundant writes are cheap.
    if any_applied {
        if let Err(e) = publish_group_info(state, mls_group_id).await {
            eprintln!("[mls] process_pending_commits: publish_group_info failed (non-fatal): {e}");
        }
    }

    // If the commit chain is ahead of where we ended up (a commit we
    // couldn't process), recover by external-joining to the latest
    // published GroupInfo. This replaces the local group state entirely,
    // jumping to the current epoch.
    let latest_remote_epoch: Option<i64> = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn
            .query(
                "SELECT epoch FROM mls_group_info WHERE conversation_id = ?1",
                libsql::params![mls_group_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Some(row.get(0)?),
            None => None,
        }
    };

    if let Some(remote_epoch) = latest_remote_epoch {
        if remote_epoch as u64 > current_epoch {
            eprintln!(
                "[mls] process_pending_commits: local epoch {current_epoch} behind remote {remote_epoch} for {mls_group_id} — external-joining to recover"
            );
            if let Err(e) = external_join_group(state, mls_group_id, user_id).await {
                eprintln!(
                    "[mls] process_pending_commits: external_join_group recovery failed for {mls_group_id}: {e}"
                );
            }
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
    user_id: String,
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
    process_pending_commits_inner(state.inner(), &mls_group_id, &user_id).await
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

    // 2. Purge stale commit log and welcome entries so process_pending_commits
    //    doesn't try to apply old-generation commits against the new group.
    let conn = state.remote_db.conn().await?;
    let _ = conn.execute(
        "DELETE FROM mls_commit_log WHERE conversation_id = ?1",
        libsql::params![mls_group_id],
    ).await;
    let _ = conn.execute(
        "DELETE FROM mls_welcome WHERE conversation_id = ?1 AND delivered = 0",
        libsql::params![mls_group_id],
    ).await;
    drop(conn);

    // 3. Reconcile adds all roster members' available devices in one commit.
    let outcome = reconcile_group_mls_impl(state, mls_group_id, sender_id).await?;
    eprintln!(
        "[mls] repair: done — {} devices added, {} removed",
        outcome.added.len(),
        outcome.removed.len(),
    );
    Ok(())
}

// ── Declarative reconcile ────────────────────────────────────────────────────

/// Outcome of a single reconcile pass.
#[derive(Debug, Default)]
pub struct ReconcileOutcome {
    /// `(user_id, device_id)` pairs added to the MLS tree.
    pub added: Vec<(String, String)>,
    /// `(user_id, device_id)` pairs removed from the MLS tree.
    pub removed: Vec<(String, String)>,
    pub epoch_before: u64,
    pub epoch_after: u64,
    /// True if the committer's own leaf was in `to_remove` and was skipped.
    pub skipped_self_removal: bool,
}

/// Raw bytes produced by a reconcile commit, needed for posting to Turso.
pub struct ReconcileCommitData {
    pub commit_bytes: Vec<u8>,
    pub welcome_bytes: Option<Vec<u8>>,
}

/// Sync core: computes the diff between the desired roster and the actual MLS
/// tree, then issues a single combined commit. Testable without Turso or async.
///
/// Returns the outcome plus optional commit/welcome bytes for the caller to
/// post to Turso. The commit is merged locally before returning.
///
/// # Arguments
/// - `roster_user_ids` — set of user_ids that SHOULD be in the group (from Turso)
/// - `available_kps` — `(user_id, device_id, KeyPackage)` for devices that have
///   an unclaimed KP and can be added
/// - `actor_user_id` / `actor_device_id` — the committer (must already be in the tree)
pub fn reconcile_group_mls_core(
    provider: &PollisProvider<'_>,
    signer: &SignatureKeyPair,
    group: &mut MlsGroup,
    roster_user_ids: &std::collections::HashSet<String>,
    available_kps: &[(String, String, KeyPackage)],
    actor_user_id: &str,
    actor_device_id: &str,
) -> crate::error::Result<(ReconcileOutcome, Option<ReconcileCommitData>)> {
    use std::collections::{HashMap, HashSet};

    let epoch_before = group.epoch().as_u64();

    // 1. Actual state: walk the MLS tree.
    let mut actual: HashMap<(String, String), LeafNodeIndex> = HashMap::new();
    for m in group.members() {
        let uid = parse_credential_user_id(&m.credential);
        let did = parse_credential_device_id(&m.credential).unwrap_or_default();
        actual.insert((uid, did), m.index);
    }

    // 2. Build the desired set.
    //    Start with devices that have available KPs…
    let mut desired: HashSet<(String, String)> = available_kps
        .iter()
        .map(|(uid, did, _)| (uid.clone(), did.clone()))
        .collect();
    //    …UNION with existing tree members whose user is still in the roster.
    //    This prevents removing the committer's own device (which consumed its
    //    KP on creation and has none left) or other devices that are already
    //    correctly in the tree.
    for (uid, did) in actual.keys() {
        if roster_user_ids.contains(uid) {
            desired.insert((uid.clone(), did.clone()));
        }
    }

    // 3. Diff.
    let actual_keys: HashSet<(String, String)> = actual.keys().cloned().collect();

    // Leaves in tree but not desired → remove
    let mut to_remove: Vec<((String, String), LeafNodeIndex)> = actual
        .iter()
        .filter(|(key, _)| !desired.contains(key))
        .map(|(key, &idx)| (key.clone(), idx))
        .collect();

    // Devices desired but not in tree → add
    let to_add_keys: HashSet<(String, String)> = desired
        .difference(&actual_keys)
        .cloned()
        .collect();

    // 4. Committer-in-remove-set detection.
    let mut skipped_self_removal = false;
    let actor_key = (actor_user_id.to_string(), actor_device_id.to_string());
    if to_remove.iter().any(|(key, _)| key == &actor_key) {
        to_remove.retain(|(key, _)| key != &actor_key);
        skipped_self_removal = true;
    }

    // Collect validated KPs for the add set.
    let add_kps: Vec<(String, String, KeyPackage)> = available_kps
        .iter()
        .filter(|(uid, did, _)| to_add_keys.contains(&(uid.clone(), did.clone())))
        .cloned()
        .collect();

    let remove_indices: Vec<LeafNodeIndex> = to_remove.iter().map(|(_, idx)| *idx).collect();

    // 5. No-op check.
    if remove_indices.is_empty() && add_kps.is_empty() {
        return Ok((
            ReconcileOutcome {
                epoch_before,
                epoch_after: epoch_before,
                skipped_self_removal,
                ..Default::default()
            },
            None,
        ));
    }

    // 6. Log the diff.
    let removed_desc: Vec<String> = to_remove.iter().map(|((u, d), _)| format!("{u}:{d}")).collect();
    let added_desc: Vec<String> = add_kps.iter().map(|(u, d, _)| format!("{u}:{d}")).collect();
    eprintln!(
        "[mls] reconcile: removing [{}], adding [{}]",
        removed_desc.join(", "),
        added_desc.join(", "),
    );

    // 7. Build a single commit with both proposals.
    let add_kps_only: Vec<KeyPackage> = add_kps.iter().map(|(_, _, kp)| kp.clone()).collect();

    let bundle = group
        .commit_builder()
        .propose_removals(remove_indices.iter().copied())
        .propose_adds(add_kps_only.into_iter())
        .load_psks(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile load_psks: {e}")))?
        .build(provider.rand(), provider.crypto(), signer, |_| true)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile build: {e}")))?
        .stage_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile stage: {e}")))?;

    // 8. Serialize commit + welcome BEFORE merging.
    let (commit_out, welcome_opt, _group_info) = bundle.into_messages();

    let commit_bytes = commit_out
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile commit serialize: {e}")))?;

    let welcome_bytes = match welcome_opt {
        Some(w) => Some(
            w.tls_serialize_detached()
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile welcome serialize: {e}")))?,
        ),
        None => None,
    };

    // 9. Merge the commit locally.
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile merge: {e}")))?;

    let epoch_after = group.epoch().as_u64();

    let removed: Vec<(String, String)> = to_remove.into_iter().map(|(key, _)| key).collect();
    let added: Vec<(String, String)> = add_kps.into_iter().map(|(u, d, _)| (u, d)).collect();

    eprintln!(
        "[mls] reconcile: epoch {epoch_before} → {epoch_after}, removed {}, added {}",
        removed.len(),
        added.len(),
    );

    Ok((
        ReconcileOutcome {
            added,
            removed,
            epoch_before,
            epoch_after,
            skipped_self_removal,
        },
        Some(ReconcileCommitData {
            commit_bytes,
            welcome_bytes,
        }),
    ))
}

/// Async entry point: reads desired state from Turso, loads local MLS group,
/// calls `reconcile_group_mls_core`, posts commit + welcome rows.
pub async fn reconcile_group_mls_impl(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<ReconcileOutcome> {
    let conversation_id = conversation_id.to_owned();
    let actor_user_id = actor_user_id.to_owned();

    let conn = state.remote_db.conn().await?;

    // 1. Determine roster: group_member + pending invitees, or dm_channel_member.
    //    Pending invitees are included so their devices get a Welcome at invite
    //    time — the acceptor can join the MLS group without requiring any other
    //    member to be online simultaneously.
    let mut roster_user_ids = std::collections::HashSet::new();
    {
        let mut rows = conn
            .query(
                "SELECT user_id FROM group_member WHERE group_id = ?1",
                libsql::params![conversation_id.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }
    // Include pending invitees so they receive a Welcome pre-acceptance.
    {
        let mut rows = conn
            .query(
                "SELECT invitee_id FROM group_invite WHERE group_id = ?1",
                libsql::params![conversation_id.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }
    if roster_user_ids.is_empty() {
        let mut rows = conn
            .query(
                "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1",
                libsql::params![conversation_id.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }

    // 2. Find devices with unclaimed KPs for all roster users.
    let mut device_pairs: Vec<(String, String)> = Vec::new();
    {
        let safe_ids: Vec<String> = roster_user_ids
            .iter()
            .map(|id| id.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
            .collect();
        if !safe_ids.is_empty() {
            let in_clause = safe_ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(",");
            let query = format!(
                "SELECT d.user_id, d.device_id FROM user_device d \
                 WHERE d.user_id IN ({in_clause}) \
                 AND EXISTS ( \
                     SELECT 1 FROM mls_key_package kp \
                     WHERE kp.user_id = d.user_id AND kp.device_id = d.device_id AND kp.claimed = 0 \
                 )"
            );
            let mut rows = conn.query(&query, ()).await?;
            while let Some(row) = rows.next().await? {
                device_pairs.push((row.get::<String>(0)?, row.get::<String>(1)?));
            }
        }
    }

    let actor_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .unwrap_or_default();

    // 3. Peek at the current tree to learn which devices are already members.
    //    This lets us skip claiming KPs for devices that don't need to be added,
    //    avoiding unnecessary KP exhaustion on repeated reconciles.
    let already_in_tree: std::collections::HashSet<(String, String)> = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        match MlsGroup::load(provider.storage(), &group_id) {
            Ok(Some(group)) => group
                .members()
                .map(|m| {
                    let uid = parse_credential_user_id(&m.credential);
                    let did = parse_credential_device_id(&m.credential).unwrap_or_default();
                    (uid, did)
                })
                .collect(),
            _ => {
                return Ok(ReconcileOutcome::default());
            }
        }
    };

    // Only claim KPs for devices not already in the tree.
    let devices_to_claim: Vec<(String, String)> = device_pairs
        .into_iter()
        .filter(|pair| !already_in_tree.contains(pair))
        .collect();

    // 4. Claim one KP per device that needs to be added.
    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (uid, did) in &devices_to_claim {
        let mut rows = conn
            .query(
                "UPDATE mls_key_package \
                 SET claimed = 1 \
                 WHERE ref_hash = ( \
                     SELECT ref_hash FROM mls_key_package \
                     WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 \
                     ORDER BY created_at ASC LIMIT 1 \
                 ) \
                 RETURNING key_package",
                libsql::params![uid.clone(), did.clone()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            kp_tuples.push((uid.clone(), did.clone(), row.get::<Vec<u8>>(0)?));
        }
    }

    // 5. Validate KPs and call the sync core under the local_db lock.
    let (outcome, commit_data_opt) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };
        let provider = PollisProvider::new(db.conn());

        // Load group — early return if missing.
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        let group_opt = MlsGroup::load(provider.storage(), &group_id)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?;
        let mut group = match group_opt {
            Some(g) => g,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };

        // Read signer.
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

        // Resolve pending commit.
        group
            .merge_pending_commit(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge pending: {e}")))?;

        // Validate KPs.
        let mut available_kps: Vec<(String, String, KeyPackage)> = Vec::new();
        for (uid, did, kp_raw) in &kp_tuples {
            let mut reader: &[u8] = kp_raw;
            let kp_in = match KeyPackageIn::tls_deserialize(&mut reader) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("[mls] reconcile: kp deserialize failed for {uid}:{did}: {e}");
                    continue;
                }
            };
            let kp = match kp_in.validate(provider.crypto(), ProtocolVersion::Mls10) {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("[mls] reconcile: kp validate failed for {uid}:{did}: {e}");
                    continue;
                }
            };
            let cred_user = parse_credential_user_id(kp.leaf_node().credential());
            if cred_user != *uid {
                eprintln!("[mls] reconcile: credential user '{cred_user}' != '{uid}' for device {did}");
                continue;
            }
            available_kps.push((uid.clone(), did.clone(), kp));
        }

        reconcile_group_mls_core(
            &provider,
            &signer,
            &mut group,
            &roster_user_ids,
            &available_kps,
            &actor_user_id,
            &actor_device_id,
        )?
    };

    // 5. Post commit + welcome to Turso.
    if let Some(data) = commit_data_opt {
        // Collect metadata about added devices so receivers can verify
        // cross-signing certs before processing the commit.
        let (added_uid, added_dids): (Option<String>, Option<String>) = if outcome.added.is_empty() {
            (None, None)
        } else {
            // All adds in one reconcile commit target devices of different
            // users, so we record the first user and all device IDs. For
            // single-user adds (the common case) this is exact.
            let uid = outcome.added[0].0.clone();
            let dids = outcome
                .added
                .iter()
                .map(|(_, d)| d.as_str())
                .collect::<Vec<_>>()
                .join(",");
            (Some(uid), Some(dids))
        };
        conn.execute(
            "INSERT INTO mls_commit_log \
             (conversation_id, epoch, sender_id, commit_data, added_user_id, added_device_ids) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![
                conversation_id.clone(),
                outcome.epoch_before as i64,
                actor_user_id,
                data.commit_bytes,
                added_uid,
                added_dids
            ],
        )
        .await?;

        if let Some(welcome_bytes) = data.welcome_bytes {
            for (uid, did) in &outcome.added {
                let welcome_id = Ulid::new().to_string();
                conn.execute(
                    "INSERT INTO mls_welcome (id, conversation_id, recipient_id, recipient_device_id, welcome_data) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    libsql::params![welcome_id, conversation_id.clone(), uid.clone(), did.clone(), welcome_bytes.clone()],
                )
                .await?;
            }
        }

        // Publish updated GroupInfo so external-join (new device enrollment)
        // uses the latest tree state.
        if let Err(e) = publish_group_info(state, &conversation_id).await {
            eprintln!("[mls] reconcile: publish_group_info failed (non-fatal): {e}");
        }
    }

    Ok(outcome)
}

#[tauri::command]
pub async fn reconcile_group_mls(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    actor_user_id: String,
) -> crate::error::Result<()> {
    reconcile_group_mls_impl(state.inner(), &conversation_id, &actor_user_id).await?;
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
    /// channels the user belongs to and broadcasts `membership_changed` for
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
        // groups and broadcasting membership_changed for each. Here we do
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

    // ── multi-device helpers ────────────────────────────────────────────────

    /// Create an MLS group with an explicit device_id in the credential.
    fn create_group_with_device(
        conn: &rusqlite::Connection,
        conversation_id: &str,
        user_id: &str,
        device_id: &str,
    ) -> SignatureKeyPair {
        let provider = PollisProvider::new(conn);
        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        sig_keys.store(provider.storage()).unwrap();

        let credential = make_credential(user_id, device_id);
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

    /// Generate a key package with an explicit device_id in the credential.
    fn gen_key_package_with_device(
        conn: &rusqlite::Connection,
        user_id: &str,
        device_id: &str,
    ) -> Vec<u8> {
        let provider = PollisProvider::new(conn);
        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        sig_keys.store(provider.storage()).unwrap();

        let credential = make_credential(user_id, device_id);
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

    /// Generate a key package using a pre-existing signing keypair. This
    /// simulates the production "stable per-device signing key" from
    /// `load_or_create_device_signer`: every KP the device publishes is
    /// signed by the same key, so the credential's signature key bytes
    /// match on every re-issue.
    fn gen_key_package_with_existing_signer(
        conn: &rusqlite::Connection,
        user_id: &str,
        device_id: &str,
        sig_keys: &SignatureKeyPair,
    ) -> Vec<u8> {
        let provider = PollisProvider::new(conn);

        let credential = make_credential(user_id, device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_keys.to_public_vec().into(),
            CS.signature_algorithm(),
        ).unwrap();
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let bundle = KeyPackage::builder()
            .build(CS, &provider, sig_keys, cred_with_key)
            .unwrap();

        bundle.key_package().tls_serialize_detached().unwrap()
    }

    /// Add multiple key packages to a group in a single `add_members` call.
    /// Mirrors production reconcile which batches all devices.
    fn add_members_batch(
        adder_db: &rusqlite::Connection,
        conv_id: &str,
        kp_bytes_list: &[Vec<u8>],
    ) -> (Vec<u8>, Vec<u8>) {
        let provider = PollisProvider::new(adder_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

        let validated_kps: Vec<KeyPackage> = kp_bytes_list.iter().map(|kp_raw| {
            let mut reader: &[u8] = kp_raw;
            let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
            kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap()
        }).collect();

        let (commit_msg, welcome_msg, _) =
            group.add_members(&provider, &signer, &validated_kps).unwrap();
        group.merge_pending_commit(&provider).unwrap();

        (
            commit_msg.tls_serialize_detached().unwrap(),
            welcome_msg.tls_serialize_detached().unwrap(),
        )
    }

    // ── multi-device credential tests ───────────────────────────────────────

    /// Credential encodes user_id:device_id; parsing extracts user_id.
    #[test]
    fn credential_roundtrip_with_device() {
        let cred = make_credential("alice", "dev_01ABCDEF");
        assert_eq!(parse_credential_user_id(&cred), "alice");
        assert_eq!(
            String::from_utf8_lossy(cred.serialized_content()),
            "alice:dev_01ABCDEF"
        );
    }

    /// Legacy credentials (no colon) still parse correctly.
    #[test]
    fn credential_legacy_no_device_id() {
        let cred: Credential = BasicCredential::new("alice".as_bytes().to_vec()).into();
        assert_eq!(parse_credential_user_id(&cred), "alice");
    }

    // ── multi-device scenario tests ─────────────────────────────────────────

    /// Alice has two devices in the group. Bob sends a message. Both Alice
    /// devices decrypt it.
    #[test]
    fn multi_device_both_devices_decrypt() {
        let conv_id = "01JTEST00000000000MULTIDEV1";

        let alice_d1_db = make_db();
        let alice_d2_db = make_db();
        let bob_db = make_db();

        // Alice device 1 creates the group.
        create_group_with_device(&alice_d1_db, conv_id, "alice", "alice_d1");

        // Add Bob.
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (add_bob_commit, bob_welcome) =
            add_member_to_group(&alice_d1_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);
        let _ = add_bob_commit;

        // Add Alice's second device.
        let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");
        let (add_d2_commit, alice_d2_welcome) =
            add_member_to_group(&alice_d1_db, conv_id, &alice_d2_kp);
        join_via_welcome(&alice_d2_db, &alice_d2_welcome);
        apply_commit(&bob_db, conv_id, &add_d2_commit);

        // Bob sends.
        let bob_ct = try_mls_encrypt(&bob_db, conv_id, b"hello both alices").unwrap();

        // Both Alice devices decrypt.
        assert_eq!(
            try_mls_decrypt(&alice_d1_db, conv_id, &bob_ct).unwrap(),
            b"hello both alices"
        );
        assert_eq!(
            try_mls_decrypt(&alice_d2_db, conv_id, &bob_ct).unwrap(),
            b"hello both alices"
        );
    }

    /// Two devices for the same user are added in a single add_members commit
    /// (matching production reconcile). Both join via the same
    /// Welcome and can decrypt.
    #[test]
    fn multi_device_batch_add_single_commit() {
        let conv_id = "01JTEST00000000000MULTIDEV2";

        let alice_db = make_db();
        let bob_d1_db = make_db();
        let bob_d2_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        // Generate KPs for both Bob devices.
        let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
        let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");

        // Add both in a single commit.
        let (_commit, welcome) =
            add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);

        // Both Bob devices process the same Welcome.
        join_via_welcome(&bob_d1_db, &welcome);
        join_via_welcome(&bob_d2_db, &welcome);

        // Alice sends — both Bob devices decrypt.
        let ct = try_mls_encrypt(&alice_db, conv_id, b"hello bob devices").unwrap();
        assert_eq!(
            try_mls_decrypt(&bob_d1_db, conv_id, &ct).unwrap(),
            b"hello bob devices"
        );
        assert_eq!(
            try_mls_decrypt(&bob_d2_db, conv_id, &ct).unwrap(),
            b"hello bob devices"
        );

        // Bob device 1 sends — Alice and Bob device 2 both decrypt.
        let bob_ct = try_mls_encrypt(&bob_d1_db, conv_id, b"from bob d1").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_db, conv_id, &bob_ct).unwrap(),
            b"from bob d1"
        );
        assert_eq!(
            try_mls_decrypt(&bob_d2_db, conv_id, &bob_ct).unwrap(),
            b"from bob d1"
        );
    }

    /// Removing a user removes ALL their device leaf nodes. Neither device
    /// can decrypt messages from the new epoch.
    #[test]
    fn remove_multi_device_user_removes_all_leaves() {
        let conv_id = "01JTEST00000000000MULTIDEV3";

        let alice_db = make_db();
        let bob_d1_db = make_db();
        let bob_d2_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        // Add both Bob devices in one commit.
        let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
        let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");
        let (_commit, welcome) =
            add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);
        join_via_welcome(&bob_d1_db, &welcome);
        join_via_welcome(&bob_d2_db, &welcome);

        // Both can decrypt before removal.
        let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"before removal").unwrap();
        assert!(try_mls_decrypt(&bob_d1_db, conv_id, &pre_ct).is_some());
        assert!(try_mls_decrypt(&bob_d2_db, conv_id, &pre_ct).is_some());

        // Alice removes "bob" — removes both leaf nodes.
        let _remove_commit = remove_member(&alice_db, conv_id, "bob");

        // Alice sends at new epoch.
        let post_ct = try_mls_encrypt(&alice_db, conv_id, b"after removal").unwrap();

        // Neither Bob device can decrypt.
        assert!(
            try_mls_decrypt(&bob_d1_db, conv_id, &post_ct).is_none(),
            "bob device 1 must not decrypt after removal"
        );
        assert!(
            try_mls_decrypt(&bob_d2_db, conv_id, &post_ct).is_none(),
            "bob device 2 must not decrypt after removal"
        );
    }

    /// A second device joins later. It cannot read pre-join history but can
    /// decrypt new messages.
    #[test]
    fn second_device_joins_later_cannot_read_history() {
        let conv_id = "01JTEST00000000000MULTIDEV4";

        let alice_d1_db = make_db();
        let alice_d2_db = make_db();
        let bob_db = make_db();

        // Alice device 1 creates group and adds Bob.
        create_group_with_device(&alice_d1_db, conv_id, "alice", "alice_d1");
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (_, bob_welcome) = add_member_to_group(&alice_d1_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Messages sent before alice_d2 joins.
        let history_ct = try_mls_encrypt(&bob_db, conv_id, b"history msg").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_d1_db, conv_id, &history_ct).unwrap(),
            b"history msg"
        );

        // Alice device 2 joins.
        let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");
        let (add_d2_commit, alice_d2_welcome) =
            add_member_to_group(&alice_d1_db, conv_id, &alice_d2_kp);
        join_via_welcome(&alice_d2_db, &alice_d2_welcome);
        apply_commit(&bob_db, conv_id, &add_d2_commit);

        // Device 2 cannot read pre-join history.
        assert!(
            try_mls_decrypt(&alice_d2_db, conv_id, &history_ct).is_none(),
            "second device must not decrypt messages from before it joined"
        );

        // Both devices decrypt new messages.
        let new_ct = try_mls_encrypt(&bob_db, conv_id, b"new msg").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_d1_db, conv_id, &new_ct).unwrap(),
            b"new msg"
        );
        assert_eq!(
            try_mls_decrypt(&alice_d2_db, conv_id, &new_ct).unwrap(),
            b"new msg"
        );
    }

    /// Regression test for the "re-invited user cannot send messages" bug.
    ///
    /// Scenario: Bob joins, Bob leaves (local-only — no remove commit is
    /// posted, which is what production `leave_group` actually does), Alice
    /// re-invites Bob. Because Bob's device signing key is STABLE across
    /// re-enrollments, a naive `add_members` call fails with
    /// `validate_key_uniqueness` — Bob's new leaf signing key is already in
    /// Alice's tree. The fix: detect the stale leaf and issue a combined
    /// remove+add commit via `commit_builder` (reconcile handles this).
    #[test]
    fn reinvite_with_stable_signing_key_handles_stale_leaf() {
        let conv_id = "01JTEST000000000REINVITE01";

        let alice_db = make_db();
        let bob_db = make_db();

        // Alice creates the group.
        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        // Bob's device picks a stable signing key it will reuse across
        // enrollments (simulates `load_or_create_device_signer`).
        let bob_signer = {
            let provider = PollisProvider::new(&bob_db);
            let sk = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
            sk.store(provider.storage()).unwrap();
            sk
        };

        // First enrollment: Bob publishes a KP, Alice adds Bob, Bob joins.
        let bob_kp_v1 =
            gen_key_package_with_existing_signer(&bob_db, "bob", "bob_d1", &bob_signer);
        let (_add1_commit, welcome1) = add_members_batch(&alice_db, conv_id, &[bob_kp_v1]);
        join_via_welcome(&bob_db, &welcome1);

        // Verify Alice and Bob can talk.
        let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"first life").unwrap();
        assert_eq!(
            try_mls_decrypt(&bob_db, conv_id, &pre_ct).unwrap(),
            b"first life"
        );

        // Bob "leaves" — in production this wipes Bob's local state but does
        // NOT post a remove commit to the group. Alice still has Bob's leaf.
        // We simulate this by leaving Alice's view untouched and dropping
        // Bob's local group state on the floor.

        // Second enrollment: Bob publishes a NEW KP using the SAME signer.
        let bob_db_v2 = make_db();
        let bob_signer_v2 = {
            let provider = PollisProvider::new(&bob_db_v2);
            let sk = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
            sk.store(provider.storage()).unwrap();
            sk
        };

        // Store bob_signer's keypair into bob_db_v2 for Bob to decrypt the welcome.
        {
            let provider_v2 = PollisProvider::new(&bob_db_v2);
            bob_signer.store(provider_v2.storage()).unwrap();
        }
        let _ = bob_signer_v2;
        let bob_kp_v2 =
            gen_key_package_with_existing_signer(&bob_db_v2, "bob", "bob_d1", &bob_signer);

        // Plain add_members should fail: the signing key is already in the
        // tree from bob_kp_v1's still-present leaf.
        {
            let provider = PollisProvider::new(&alice_db);
            let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();
            let mut reader: &[u8] = &bob_kp_v2;
            let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
            let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();
            let naive = group.add_members(&provider, &signer, &[kp]);
            assert!(
                naive.is_err(),
                "plain add_members must reject duplicate signing key — if this \
                 starts passing, openmls has changed validation and the stale-leaf \
                 branch in reconcile_group_mls_core may no longer be needed"
            );
        }

        // Apply the fix: combined remove+add commit via commit_builder.
        let welcome2_bytes: Vec<u8> = {
            let provider = PollisProvider::new(&alice_db);
            let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

            // Find Bob's stale leaves.
            let stale: Vec<LeafNodeIndex> = group.members()
                .filter_map(|m| {
                    let u = parse_credential_user_id(&m.credential);
                    let d = parse_credential_device_id(&m.credential)?;
                    if u == "bob" && d == "bob_d1" {
                        Some(m.index)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(stale.len(), 1, "expected exactly one stale bob leaf");

            let mut reader: &[u8] = &bob_kp_v2;
            let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
            let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();

            let bundle = group
                .commit_builder()
                .propose_removals(stale.iter().cloned())
                .propose_adds(std::iter::once(kp))
                .load_psks(provider.storage())
                .unwrap()
                .build(provider.rand(), provider.crypto(), &signer, |_| true)
                .unwrap()
                .stage_commit(&provider)
                .unwrap();

            let (_commit, welcome_opt, _gi) = bundle.into_messages();
            let welcome = welcome_opt.expect("welcome must be produced by add proposal");
            group.merge_pending_commit(&provider).unwrap();
            welcome.tls_serialize_detached().unwrap()
        };

        // Bob (new local state) joins via the fresh welcome.
        join_via_welcome(&bob_db_v2, &welcome2_bytes);

        // Alice and Bob-v2 can now send and receive.
        let hello = try_mls_encrypt(&alice_db, conv_id, b"welcome back bob").unwrap();
        assert_eq!(
            try_mls_decrypt(&bob_db_v2, conv_id, &hello).unwrap(),
            b"welcome back bob"
        );
        let reply = try_mls_encrypt(&bob_db_v2, conv_id, b"thanks alice").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_db, conv_id, &reply).unwrap(),
            b"thanks alice"
        );
    }

    // ── reconcile core tests ────────────────────────────────────────────────

    /// Helper: call `reconcile_group_mls_core` with pre-constructed inputs.
    /// Validates raw KP bytes and delegates to the sync core.
    fn reconcile(
        actor_db: &rusqlite::Connection,
        conv_id: &str,
        roster_user_ids: &[&str],
        available_kp_bytes: &[(String, String, Vec<u8>)],
        actor_user_id: &str,
        actor_device_id: &str,
    ) -> (ReconcileOutcome, Option<ReconcileCommitData>) {
        let provider = PollisProvider::new(actor_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

        let roster: std::collections::HashSet<String> =
            roster_user_ids.iter().map(|s| s.to_string()).collect();

        let available_kps: Vec<(String, String, KeyPackage)> = available_kp_bytes
            .iter()
            .map(|(uid, did, bytes)| {
                let mut reader: &[u8] = bytes;
                let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
                let kp = kp_in
                    .validate(provider.crypto(), ProtocolVersion::Mls10)
                    .unwrap();
                (uid.clone(), did.clone(), kp)
            })
            .collect();

        reconcile_group_mls_core(
            &provider,
            &signer,
            &mut group,
            &roster,
            &available_kps,
            actor_user_id,
            actor_device_id,
        )
        .unwrap()
    }

    /// 1. Clean state: tree matches roster, no available KPs → no-op.
    #[test]
    fn reconcile_no_drift() {
        let conv_id = "01JTEST00000000000RECONCILE1";
        let alice_db = make_db();
        let bob_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice", "bob"],
            &[],
            "alice",
            "alice_d1",
        );

        assert!(outcome.added.is_empty());
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.epoch_before, outcome.epoch_after);
        assert!(!outcome.skipped_self_removal);
        assert!(commit_data.is_none());
    }

    /// 2. Stale leaf: user removed from roster but leaf remains → remove.
    #[test]
    fn reconcile_remove_stale_leaf() {
        let conv_id = "01JTEST00000000000RECONCILE2";
        let alice_db = make_db();
        let bob_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice"],
            &[],
            "alice",
            "alice_d1",
        );

        assert_eq!(outcome.removed.len(), 1);
        assert_eq!(
            outcome.removed[0],
            ("bob".to_string(), "bob_d1".to_string())
        );
        assert!(outcome.added.is_empty());
        assert!(outcome.epoch_after > outcome.epoch_before);
        assert!(commit_data.is_some());
    }

    /// 3. Missing device: user in roster with available KP but not in tree → add.
    #[test]
    fn reconcile_add_missing_device() {
        let conv_id = "01JTEST00000000000RECONCILE3";
        let alice_db = make_db();
        let bob_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        let bob_kp_bytes = gen_key_package_with_device(&bob_db, "bob", "bob_d1");

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice", "bob"],
            &[("bob".into(), "bob_d1".into(), bob_kp_bytes)],
            "alice",
            "alice_d1",
        );

        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.added.len(), 1);
        assert_eq!(
            outcome.added[0],
            ("bob".to_string(), "bob_d1".to_string())
        );
        assert!(outcome.epoch_after > outcome.epoch_before);
        let data = commit_data.unwrap();
        assert!(
            data.welcome_bytes.is_some(),
            "additions must produce a Welcome"
        );
    }

    /// 4. Combined add + remove in a single commit.
    #[test]
    fn reconcile_combined_add_and_remove() {
        let conv_id = "01JTEST00000000000RECONCILE4";
        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        let carol_kp_bytes = gen_key_package_with_device(&carol_db, "carol", "carol_d1");

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice", "carol"],
            &[("carol".into(), "carol_d1".into(), carol_kp_bytes)],
            "alice",
            "alice_d1",
        );

        assert_eq!(outcome.removed.len(), 1);
        assert_eq!(
            outcome.removed[0],
            ("bob".to_string(), "bob_d1".to_string())
        );
        assert_eq!(outcome.added.len(), 1);
        assert_eq!(
            outcome.added[0],
            ("carol".to_string(), "carol_d1".to_string())
        );
        assert!(outcome.epoch_after > outcome.epoch_before);
        assert!(commit_data.is_some());
    }

    /// 5. Committer's own leaf is in the remove set → skipped, flag set.
    #[test]
    fn reconcile_committer_skip_self_removal() {
        let conv_id = "01JTEST00000000000RECONCILE5";
        let alice_db = make_db();
        let bob_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // Alice NOT in roster — she can't remove herself.
        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["bob"],
            &[],
            "alice",
            "alice_d1",
        );

        assert!(outcome.skipped_self_removal);
        assert!(outcome.removed.is_empty());
        assert!(outcome.added.is_empty());
        assert!(commit_data.is_none());
    }

    /// 6. Idempotence: reconcile twice with same desired state → second is no-op.
    #[test]
    fn reconcile_idempotent() {
        let conv_id = "01JTEST00000000000RECONCILE6";
        let alice_db = make_db();
        let bob_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);

        // First reconcile: remove bob.
        let (outcome1, _) = reconcile(
            &alice_db,
            conv_id,
            &["alice"],
            &[],
            "alice",
            "alice_d1",
        );
        assert_eq!(outcome1.removed.len(), 1);

        // Second reconcile: same roster → no-op.
        let (outcome2, commit_data2) = reconcile(
            &alice_db,
            conv_id,
            &["alice"],
            &[],
            "alice",
            "alice_d1",
        );
        assert!(outcome2.removed.is_empty());
        assert!(outcome2.added.is_empty());
        assert!(commit_data2.is_none());
    }

    /// 7. User in roster but not in tree and no KP → not added, no error.
    #[test]
    fn reconcile_no_kp_user_not_added() {
        let conv_id = "01JTEST00000000000RECONCILE7";
        let alice_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice", "bob"],
            &[],
            "alice",
            "alice_d1",
        );

        assert!(outcome.added.is_empty());
        assert!(outcome.removed.is_empty());
        assert!(commit_data.is_none());
    }

    /// 8. Add a second device for a user already in the tree.
    #[test]
    fn reconcile_add_second_device() {
        let conv_id = "01JTEST00000000000RECONCILE8";
        let alice_db = make_db();
        let alice_d2_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice"],
            &[("alice".into(), "alice_d2".into(), alice_d2_kp)],
            "alice",
            "alice_d1",
        );

        assert_eq!(outcome.added.len(), 1);
        assert_eq!(
            outcome.added[0],
            ("alice".to_string(), "alice_d2".to_string())
        );
        assert!(outcome.removed.is_empty());
        assert!(commit_data.is_some());
    }

    /// 9. Remove all leaves for a multi-device user removed from roster.
    #[test]
    fn reconcile_remove_multi_device_user() {
        let conv_id = "01JTEST00000000000RECONCILE9";
        let alice_db = make_db();
        let bob_d1_db = make_db();
        let bob_d2_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
        let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");
        let (_, welcome) = add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);
        join_via_welcome(&bob_d1_db, &welcome);
        join_via_welcome(&bob_d2_db, &welcome);

        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice"],
            &[],
            "alice",
            "alice_d1",
        );

        assert_eq!(outcome.removed.len(), 2);
        let removed_ids: std::collections::HashSet<(String, String)> =
            outcome.removed.into_iter().collect();
        assert!(removed_ids.contains(&("bob".to_string(), "bob_d1".to_string())));
        assert!(removed_ids.contains(&("bob".to_string(), "bob_d2".to_string())));
        assert!(outcome.added.is_empty());
        assert!(commit_data.is_some());
    }

    /// 10. End-to-end: reconcile removes a stale leaf, remaining members apply
    /// the commit, and the removed member cannot decrypt new messages.
    #[test]
    fn reconcile_e2e_remove_then_communicate() {
        let conv_id = "01JTEST00000000000RECONCILEA";
        let alice_db = make_db();
        let bob_db = make_db();
        let carol_db = make_db();

        create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

        let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
        let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
        join_via_welcome(&bob_db, &bob_welcome);
        let _ = add_bob_commit;

        let carol_kp = gen_key_package_with_device(&carol_db, "carol", "carol_d1");
        let (add_carol_commit, carol_welcome) =
            add_member_to_group(&alice_db, conv_id, &carol_kp);
        join_via_welcome(&carol_db, &carol_welcome);
        apply_commit(&bob_db, conv_id, &add_carol_commit);

        // Bob "leaves" — removed from roster. Alice reconciles.
        let (outcome, commit_data) = reconcile(
            &alice_db,
            conv_id,
            &["alice", "carol"],
            &[],
            "alice",
            "alice_d1",
        );

        assert_eq!(outcome.removed.len(), 1);
        assert_eq!(
            outcome.removed[0],
            ("bob".to_string(), "bob_d1".to_string())
        );

        // Carol applies the reconcile commit.
        let data = commit_data.unwrap();
        apply_commit(&carol_db, conv_id, &data.commit_bytes);

        // Alice and Carol can communicate.
        let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
        assert_eq!(
            try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap(),
            b"bob is gone"
        );

        let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"confirmed").unwrap();
        assert_eq!(
            try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap(),
            b"confirmed"
        );

        // Bob cannot decrypt post-reconcile messages.
        assert!(
            try_mls_decrypt(&bob_db, conv_id, &alice_ct).is_none(),
            "removed member must not decrypt after reconcile"
        );
    }
}
