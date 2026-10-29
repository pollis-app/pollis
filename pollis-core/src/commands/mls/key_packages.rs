//! MLS key-package lifecycle.
//!
//! The KeyPackage pool: rotated on login, topped up when the Welcome drain reports it low (#1412).
//!
//! ## The last-resort package (#1571)
//!
//! Beside the [`POOL_TARGET`] one-shot packages, a device publishes ONE last-resort package per suite (RFC 9420 §16.8), handed out only when the pool is empty and never consumed.
//!
//! Login rotation replaces it; a top-up publishes one only on `has_last_resort == Some(false)`.

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

/// Shape `(ref_hash, key_package_bytes)` pairs into the DS `packages` array (base64).
///
/// `ciphersuite` lets the DS narrow a claim by suite; it cannot read the opaque blob.
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
            last_resort: false,
        })
        .collect()
}

/// The wire entry for this device's last-resort package (#1571).
fn last_resort_json(
    pair: &(String, Vec<u8>),
    ciphersuite: Ciphersuite,
) -> pollis_api::devices::KeyPackageEntry {
    let mut entry = kp_packages_json(std::slice::from_ref(pair), ciphersuite)
        .pop()
        .expect("one pair in, one entry out");
    entry.last_resort = true;
    entry
}

/// Build one fresh `KeyPackage` in an explicit ciphersuite, returning
/// `(ref_hex, tls_bytes)`.
///
/// Sync: the caller owns the local-DB guard. Tests' shorthand for [`build_key_package`].
#[cfg(test)]
pub(super) fn build_key_package_in_suite<C>(
    provider: &MlsProvider<'_, C>,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
) -> Result<(String, Vec<u8>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    build_key_package(provider, user_id, device_id, suite, false)
}

/// [`build_key_package_in_suite`], optionally as the device's LAST-RESORT
/// package (#1571): the same package with openmls's `LastResort` extension,
/// which is what makes openmls keep its private key after a Welcome uses it.
pub(super) fn build_key_package<C>(
    provider: &MlsProvider<'_, C>,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
    last_resort: bool,
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

    let mut builder = KeyPackage::builder();
    if last_resort {
        // RFC 9420 §7.2: the leaf must list every extension it carries, or `KeyPackageIn::validate` refuses it.
        builder = builder
            .leaf_node_capabilities(
                Capabilities::builder()
                    .extensions(vec![ExtensionType::LastResort])
                    .build(),
            )
            .mark_as_last_resort();
    }
    let bundle = builder
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

/// Test-harness only: a current-suite KeyPackage claiming `claimed_user_id:claimed_device_id` but signed by an uncertified key, as a malicious DS would serve.
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

/// Build one fresh `KeyPackage` for this device in `suite`.
async fn build_one_key_package_in_suite(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
) -> Result<(String, Vec<u8>)> {
    build_one(state, user_id, device_id, suite, false).await
}

/// [`build_one_key_package_in_suite`], optionally as the last-resort package.
async fn build_one(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
    last_resort: bool,
) -> Result<(String, Vec<u8>)> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());
    build_key_package(&provider, user_id, device_id, suite, last_resort)
}

/// Rotate key packages: delete unclaimed packages for this device from the
/// remote table and publish TARGET fresh ones backed by the current local DB,
/// plus a fresh last-resort package (#1571).
///
/// Called on every login; only touches the current `device_id`'s packages.
///
/// The last-resort package rides in the same request, so the device never has neither.
pub async fn ensure_mls_key_package(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    const TARGET: i64 = POOL_TARGET;

    // Build locally first so the DS replenish is one batched call.
    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(TARGET as usize);
    for _ in 0..TARGET {
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let mut all_packages = kp_packages_json(&pairs, current_suite());
    let last_resort = build_one(state, user_id, device_id, current_suite(), true).await?;
    all_packages.push(last_resort_json(&last_resort, current_suite()));

    // Replenish clears stale unclaimed packages and inserts the pool in one owner-scoped transaction.
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
/// `reported` (the DS's unclaimed count) is the whole answer when present: packages leave the pool without a Welcome.
///
/// `None` (older DS): refill after processing a Welcome.
///
/// `has_last_resort == Some(false)` refills too, whatever the count (#1571).
pub(super) fn drain_wants_refill(
    had_welcomes: bool,
    reported: Option<i64>,
    has_last_resort: Option<bool>,
) -> bool {
    if has_last_resort == Some(false) {
        return true;
    }
    match reported {
        Some(remaining) => remaining < POOL_TARGET,
        None => had_welcomes,
    }
}

/// How many unclaimed KeyPackages a device keeps published: one is claimed for
/// every conversation the device is added to while it is away.
pub(super) const POOL_TARGET: i64 = 5;

/// Top up to [`POOL_TARGET`] without deleting; called when [`drain_wants_refill`].
///
/// Single-flight per process: concurrent drains would each overfill the pool by the shortfall.
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
    // Count only CURRENT-suite packages: a retired suite's are unclaimable.
    let (remaining, has_last_resort) =
        crate::commands::ds_reads::unclaimed_key_packages(state, suite_code).await?;

    let needed = (TARGET - remaining).max(0);
    let wants_last_resort = has_last_resort == Some(false);
    if needed == 0 && !wants_last_resort {
        return Ok(());
    }

    crate::diag!(
        "[mls] replenish: {remaining} unclaimed KPs, publishing {needed} more{}",
        if wants_last_resort { " and a last-resort package" } else { "" }
    );

    // Build the top-up packages locally before any remote write.
    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(needed as usize);
    for _ in 0..needed {
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let mut all_packages = kp_packages_json(&pairs, current_suite());
    if wants_last_resort {
        let last_resort = build_one(state, user_id, device_id, current_suite(), true).await?;
        all_packages.push(last_resort_json(&last_resort, current_suite()));
    }

    // Insert-only, so it uses the owner-scoped publish endpoint (#910).
    let body = pollis_api::devices::PublishKeyPackagesBody {
        device_id: device_id.to_string(),
        packages: all_packages,
        user_id: Some(user_id.to_string()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Validate that a `KeyPackage` blob received from the remote table is
/// well-formed and matches the expected user's credential.
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

    /// #1412: a short pool refills on any drain once the DS reports the count.
    #[test]
    fn a_reported_short_pool_is_refilled_without_a_welcome() {
        assert!(drain_wants_refill(false, Some(0), Some(true)));
        assert!(drain_wants_refill(false, Some(POOL_TARGET - 1), Some(true)));
    }

    /// #1571: a missing last-resort package refills; `None` changes nothing.
    #[test]
    fn a_missing_last_resort_package_is_published_on_the_next_drain() {
        assert!(drain_wants_refill(false, Some(POOL_TARGET), Some(false)));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET), None));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET), Some(true)));
    }

    /// A full pool is left alone even right after a Welcome: the count is the
    /// whole answer once the DS gives it.
    #[test]
    fn a_reported_full_pool_is_not_refilled() {
        assert!(!drain_wants_refill(true, Some(POOL_TARGET), Some(true)));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET + 3), Some(true)));
    }

    /// An older DS: fall back to the pre-#1412 rule.
    #[test]
    fn without_a_count_only_a_processed_welcome_refills() {
        assert!(drain_wants_refill(true, None, None));
        assert!(!drain_wants_refill(false, None, None));
    }
}
