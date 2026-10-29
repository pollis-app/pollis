//! MLS key-package lifecycle: `ensure_mls_key_package` rotates this device's
//! pool on login; `replenish_key_packages` tops it up when the Welcome drain
//! reports the pool is low (#1412).

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

/// Shape `(ref_hash, key_package_bytes)` pairs into the DS `packages` array.
///
/// `ciphersuite` is sent because the DS cannot read the suite out of the opaque
/// blob, and the column keeps a retired suite's package from being served.
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
/// `(ref_hex, tls_bytes)`. The leaf is signed with the device's stable key for
/// `suite`'s scheme (#668). Sync: the caller owns the local-DB guard.
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

/// Test-harness only: a KeyPackage whose credential claims
/// `claimed_user_id:claimed_device_id` but whose leaf is signed by a fresh,
/// uncertified key: what a malicious DS would hand a committer at claim time.
/// Only the cross-signing check can tell it apart.
#[cfg(feature = "test-harness")]
pub async fn forge_key_package_for(
    state: &Arc<AppState>,
    claimed_user_id: &str,
    claimed_device_id: &str,
) -> Result<(String, Vec<u8>)> {
    use openmls_basic_credential::SignatureKeyPair;

    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());
    let suite = current_suite();
    let scheme = signature_scheme(suite);
    let sig_keys = SignatureKeyPair::new(scheme)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("forged signer: {e}")))?;
    let sig_pub = OpenMlsSignaturePublicKey::new(sig_keys.public().into(), scheme)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
    let cred_with_key = CredentialWithKey {
        credential: make_credential(claimed_user_id, claimed_device_id),
        signature_key: sig_pub.into(),
    };
    let bundle = KeyPackage::builder()
        .build(suite, &provider, &sig_keys, cred_with_key)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp build: {e}")))?;
    let kp = bundle.key_package();
    let hash_ref = kp
        .hash_ref(provider.crypto())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp hash_ref: {e}")))?;
    let kp_bytes = kp
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("kp serialize: {e}")))?;
    Ok((hex::encode(hash_ref.as_slice()), kp_bytes))
}

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

/// Rotate key packages on login: replace this device's unclaimed packages with
/// [`POOL_TARGET`] fresh ones. Other devices' packages are left intact.
pub async fn ensure_mls_key_package(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    const TARGET: i64 = POOL_TARGET;

    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(TARGET as usize);
    for _ in 0..TARGET {
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let all_packages = kp_packages_json(&pairs, current_suite());

    // One DS transaction clears this device's unclaimed packages and inserts the
    // fresh pool, owner-scoped to the signer.
    let body = pollis_api::devices::ReplenishKeyPackagesBody {
        device_id: device_id.to_string(),
        packages: all_packages,
        user_id: Some(user_id.to_string()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Should a Welcome drain top this device's pool up (#1412)?
///
/// A reported count is the whole answer, because packages also leave the pool
/// with no Welcome (a lost-race claim, a refused leaf). `None` is an older DS:
/// keep the old rule (refill after a Welcome).
pub(super) fn drain_wants_refill(had_welcomes: bool, reported: Option<i64>) -> bool {
    match reported {
        Some(remaining) => remaining < POOL_TARGET,
        None => had_welcomes,
    }
}

/// How many unclaimed KeyPackages a device keeps published: one is claimed for
/// every conversation the device is added to while it is away.
pub(super) const POOL_TARGET: i64 = 5;

/// Top up this device's key packages to [`POOL_TARGET`] without deleting any.
///
/// Single-flight per process: concurrent drains see the same low count, and
/// publishing from each would overfill the pool once per drain.
pub(super) async fn replenish_key_packages(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    const TARGET: i64 = POOL_TARGET;

    let Ok(_refilling) = state.key_package_refill.try_lock() else {
        return Ok(());
    };

    let suite_code = i64::from(u16::from(current_suite()));
    // Count only CURRENT-suite packages, so a dead pool cannot mask an empty
    // live one. The DS scopes the count to the authenticated device.
    let remaining: i64 =
        crate::commands::ds_reads::unclaimed_key_packages(state, suite_code).await?;

    let needed = TARGET - remaining;
    if needed <= 0 {
        return Ok(());
    }

    crate::diag!("[mls] replenish: {remaining} unclaimed KPs, publishing {needed} more");

    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(needed as usize);
    for _ in 0..needed {
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let all_packages = kp_packages_json(&pairs, current_suite());

    let body = pollis_api::devices::PublishKeyPackagesBody {
        device_id: device_id.to_string(),
        packages: all_packages,
        user_id: Some(user_id.to_string()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Validate that a `KeyPackage` blob is well-formed and matches the expected
/// user's credential, returning its hex-encoded `KeyPackageRef`.
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

#[cfg(test)]
mod tests {
    use super::{drain_wants_refill, POOL_TARGET};

    /// #1412: when the DS reports the count, a short pool is refilled on any
    /// drain, Welcome or not.
    #[test]
    fn a_reported_short_pool_is_refilled_without_a_welcome() {
        assert!(drain_wants_refill(false, Some(0)));
        assert!(drain_wants_refill(false, Some(POOL_TARGET - 1)));
    }

    #[test]
    fn a_reported_full_pool_is_not_refilled() {
        assert!(!drain_wants_refill(true, Some(POOL_TARGET)));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET + 3)));
    }

    #[test]
    fn without_a_count_only_a_processed_welcome_refills() {
        assert!(drain_wants_refill(true, None));
        assert!(!drain_wants_refill(false, None));
    }
}
