//! MLS key-package lifecycle.
//!
//! The KeyPackage pool (build + publish + replenish) that precedes any MLS
//! group operation. `ensure_mls_key_package` rotates this device's pool on
//! login; `replenish_key_packages` tops it up after welcomes consume packages.
//! Both route owner-scoped writes through the Delivery Service.

use openmls::prelude::*;
use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::error::Result;
use crate::state::AppState;

use super::device::load_or_create_device_signer;
use super::provider::{
    current_suite, make_credential, parse_credential_user_id, signature_scheme, MlsProvider,
    PollisProvider,
};

// ── Key-package pool ──────────────────────────────────────────────────────────

/// Shape `(ref_hash, key_package_bytes)` pairs into the DS write-API
/// `packages` array: each `key_package` is base64 (STANDARD), since the bytes
/// are binary (the domain convention — see `pollis_delivery::devices`).
///
/// `ciphersuite` is the MLS code point every package in `pairs` was built with.
/// The DS stores it as a queryable column so a claim can be narrowed to a suite
/// — it cannot read the suite out of the opaque TLS blob. Since #669 there is
/// only one suite to narrow to, but the field is still sent: it is what makes a
/// future code-point change (see [`CS_PQ`](super::provider::CS_PQ)) a query rather than a guess, and it
/// keeps a stale package from a retired suite distinguishable from a current one.
fn kp_packages_json(
    pairs: &[(String, Vec<u8>)],
    ciphersuite: Ciphersuite,
) -> Vec<pollis_api::devices::KeyPackageEntry> {
    use base64::Engine as _;
    let code_point = i64::from(u16::from(ciphersuite));
    pairs
        .iter()
        .map(|(ref_hex, kp)| pollis_api::devices::KeyPackageEntry {
            ref_hash: ref_hex.clone(),
            key_package: base64::engine::general_purpose::STANDARD.encode(kp),
            ciphersuite: Some(code_point),
        })
        .collect()
}

/// Build one fresh `KeyPackage` in an explicit ciphersuite, returning
/// `(ref_hex, tls_bytes)`.
///
/// The package's leaf is signed with the device's stable key **for `suite`'s
/// signature scheme** (#668) — ML-DSA-44 for [`CS_PQ`](super::provider::CS_PQ). That key is stable
/// across rotations, so a device's whole pool keeps presenting the same signing
/// key. `suite` stays a parameter rather than reading `CS_PQ` directly so that a
/// code-point change needs no edit here.
///
/// Sync: the caller owns the local-DB guard.
pub(super) fn build_key_package_in_suite<C>(
    provider: &MlsProvider<'_, C>,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
) -> Result<(String, Vec<u8>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let scheme = signature_scheme(suite);
    let (sig_keys, sig_pub_bytes) =
        load_or_create_device_signer(provider, user_id, device_id, scheme)?;

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(sig_pub_bytes.into(), scheme)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(suite, provider, &sig_keys, cred_with_key)
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

/// Build one fresh `KeyPackage` for this device in `suite`, backed by the current
/// local DB. Locks the local DB for the (sync) openmls work, dropping the guard
/// before returning.
async fn build_one_key_package_in_suite(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
) -> Result<(String, Vec<u8>)> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());
    build_key_package_in_suite(&provider, user_id, device_id, suite)
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
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let all_packages = kp_packages_json(&pairs, current_suite());

    // The replenish endpoint clears this device's stale unclaimed packages and
    // inserts the fresh pool in ONE transaction, owner-scoped to the signer.
    //
    // There is no direct-to-Turso branch here any more (#910). It was the same
    // shape as `dev_login_inner`'s: a "no DS configured" fallback that performed
    // a client-side remote DELETE + INSERT, which `CLAUDE.md` bans outright. A
    // client with no DS cannot publish a device cert, send a message, or join a
    // group either, so writing key packages for it only produced a pool nothing
    // could ever claim.
    let body = pollis_api::devices::ReplenishKeyPackagesBody {
        device_id: device_id.to_string(),
        packages: all_packages,
        user_id: Some(user_id.to_string()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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

    let suite_code = i64::from(u16::from(current_suite()));
    // Count only packages in the CURRENT suite. A package left behind by a
    // retired suite is unclaimable — counting it would let a dead pool mask an
    // empty live one and silently make this device unaddable.
    //
    // Counting remaining packages is a READ — it stays direct on the local
    // libsql handle even when DS writes are enabled.
    let remaining: i64 = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND ciphersuite = ?3",
            libsql::params![user_id, device_id, suite_code],
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
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let all_packages = kp_packages_json(&pairs, current_suite());

    // A top-up is insert-only (no delete), so it routes through the owner-scoped
    // publish endpoint. As above, the direct-to-Turso fallback is gone (#910).
    let body = pollis_api::devices::PublishKeyPackagesBody {
        device_id: device_id.to_string(),
        packages: all_packages,
        user_id: Some(user_id.to_string()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

// ── Phase 2 (retained): validate_key_package ─────────────────────────────────

/// Validate that a `KeyPackage` blob received from the remote table is
/// well-formed and matches the expected user's credential.
///
/// Returns the hex-encoded `KeyPackageRef` on success so callers can store it.
///
/// Generic over the crypto backend rather than naming a concrete provider: the
/// suite a KeyPackage was built for is inside the blob, so validating one built
/// for a suite the passed backend does not implement is a *runtime* outcome,
/// and this signature keeps it one. Callers pass `provider.crypto()`.
pub fn validate_key_package(
    kp_bytes: &[u8],
    expected_user_id: &str,
    crypto: &impl OpenMlsCrypto,
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
