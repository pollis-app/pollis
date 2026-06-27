//! MLS key-package lifecycle.
//!
//! The KeyPackage pool (build + publish + replenish) that precedes any MLS
//! group operation. `ensure_mls_key_package` rotates this device's pool on
//! login; `replenish_key_packages` tops it up after welcomes consume packages.
//! Both route owner-scoped writes through the Delivery Service.

use openmls::prelude::*;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::error::Result;
use crate::state::AppState;

use super::device::load_or_create_device_signer;
use super::provider::{make_credential, parse_credential_user_id, PollisProvider, CS};

// ── Key-package pool ──────────────────────────────────────────────────────────

/// Shape `(ref_hash, key_package_bytes)` pairs into the DS write-API
/// `packages` array: each `key_package` is base64 (STANDARD), since the bytes
/// are binary (the domain convention — see `pollis_delivery::devices`).
fn kp_packages_json(pairs: &[(String, Vec<u8>)]) -> Vec<serde_json::Value> {
    use base64::Engine as _;
    pairs
        .iter()
        .map(|(ref_hex, kp)| {
            serde_json::json!({
                "ref_hash": ref_hex,
                "key_package": base64::engine::general_purpose::STANDARD.encode(kp),
            })
        })
        .collect()
}

/// Build one fresh `KeyPackage` for this device backed by the current local DB,
/// returning `(ref_hex, tls_bytes)`. All packages share the stable device
/// signing key, so a single `device_cert` covers every package this device
/// ships. Locks the local DB for the (sync) openmls work, dropping the guard
/// before returning.
async fn build_one_key_package(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<(String, Vec<u8>)> {
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

    Ok((ref_hex, kp_bytes))
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

    // Build TARGET fresh packages locally (their private keys live in the local
    // DB) before any remote write, so the DS replenish is a single batched call.
    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(TARGET as usize);
    for _ in 0..TARGET {
        pairs.push(build_one_key_package(state, user_id, device_id).await?);
    }

    // DS seam: the replenish endpoint clears this device's stale unclaimed
    // packages and inserts the fresh pool in ONE transaction (owner-scoped to the
    // signer); else do the equivalent delete-then-insert directly.
    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({
                "device_id": device_id,
                "packages": kp_packages_json(&pairs),
                "user_id": user_id,
            });
            crate::commands::mls::ds_post_ok(state, "/v1/key-packages/replenish", &body).await?;
        }
        None => {
            let conn = state.remote_db.conn().await?;
            // Remove unclaimed packages for THIS device only — their private keys
            // may no longer exist in the current local DB (e.g. after a wipe).
            // Also clean up legacy packages with NULL device_id for this user.
            conn.execute(
                "DELETE FROM mls_key_package WHERE user_id = ?1 AND claimed = 0 \
                 AND (device_id = ?2 OR device_id IS NULL)",
                libsql::params![user_id, device_id],
            ).await?;
            for (ref_hex, kp_bytes) in &pairs {
                conn.execute(
                    "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
                     VALUES (?1, ?2, ?3, ?4)",
                    libsql::params![ref_hex.clone(), user_id, kp_bytes.clone(), device_id],
                ).await?;
            }
        }
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

    // Counting remaining packages is a READ — it stays direct on the local
    // libsql handle even when DS writes are enabled.
    let remaining: i64 = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0",
            libsql::params![user_id, device_id],
        ).await?;
        let n = if let Some(row) = rows.next().await? {
            row.get(0)?
        } else {
            0
        };
        drop(rows);
        n
    };

    let needed = TARGET - remaining;
    if needed <= 0 {
        return Ok(());
    }

    eprintln!("[mls] replenish: {remaining} unclaimed KPs, publishing {needed} more");

    // Build the top-up packages locally before any remote write.
    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(needed as usize);
    for _ in 0..needed {
        pairs.push(build_one_key_package(state, user_id, device_id).await?);
    }

    // DS seam: a top-up is insert-only (no delete), so it routes through the
    // owner-scoped publish endpoint; else INSERT OR IGNORE directly.
    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({
                "device_id": device_id,
                "packages": kp_packages_json(&pairs),
                "user_id": user_id,
            });
            crate::commands::mls::ds_post_ok(state, "/v1/key-packages", &body).await?;
        }
        None => {
            let conn = state.remote_db.conn().await?;
            for (ref_hex, kp_bytes) in &pairs {
                conn.execute(
                    "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
                     VALUES (?1, ?2, ?3, ?4)",
                    libsql::params![ref_hex.clone(), user_id, kp_bytes.clone(), device_id],
                ).await?;
            }
        }
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
