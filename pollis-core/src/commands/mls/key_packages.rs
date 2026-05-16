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
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::error::Result;
use crate::state::AppState;

use super::device::load_or_create_device_signer;
use super::provider::{make_credential, parse_credential_user_id, PollisProvider, CS};

// ── Commands ──────────────────────────────────────────────────────────────────

/// Generate a fresh MLS `KeyPackage` + `SignatureKeyPair` for this device and
/// persist both in the local `mls_kv` table.
///
/// Returns the TLS-serialised `KeyPackage` bytes and its hex-encoded hash ref.
/// Safe to call multiple times — each call produces a distinct key package.
pub async fn generate_mls_key_package(
    state: &Arc<AppState>,
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
pub async fn publish_mls_key_package(
    state: &Arc<AppState>,
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
pub async fn fetch_mls_key_package(
    state: &Arc<AppState>,
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
pub(super) async fn replenish_key_packages(
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
