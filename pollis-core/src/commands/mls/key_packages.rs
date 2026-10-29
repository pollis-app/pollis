//! MLS key-package lifecycle: `ensure_mls_key_package` rotates this device's
//! pool on login; `replenish_key_packages` tops it up when the Welcome drain
//! reports it low (#1412).
//!
//! Beside the [`POOL_TARGET`] one-shot packages, each device publishes one
//! last-resort package per suite (#1571), so a device is never left with nothing
//! to be added with; see `.codesight/wiki/mls.md` → "Last-resort KeyPackage".
//! Welcomes sealed to it share an init key until the post-join self-update
//! (#666) rotates the leaf.

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

/// Shape `(ref_hash, key_package_bytes)` pairs into the DS write-API
/// `packages` array (`key_package` base64 STANDARD).
///
/// The DS cannot read the suite out of the opaque TLS blob, so `ciphersuite` is
/// sent as a queryable column: a future code-point change (see
/// [`CS_PQ`](super::provider::CS_PQ)) is then a query, and a retired suite's
/// stale package stays distinguishable.
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

/// A regular package; the unit tests' shorthand for [`build_key_package`].
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

/// Build one fresh `KeyPackage`, returning `(ref_hex, tls_bytes)`. The leaf is
/// signed with the device's stable key for `suite`'s signature scheme (#668).
/// Sync: the caller owns the local-DB guard.
///
/// `last_resort` adds openmls's `LastResort` extension, which makes openmls keep
/// the private key after a Welcome uses it (#1571).
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
        // RFC 9420 §7.2: every extension a KeyPackage carries must be listed in
        // its leaf's capabilities, or every committer's `KeyPackageIn::validate`
        // refuses it (`UnsupportedExtension`).
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

/// Test-harness only: a KeyPackage whose credential claims
/// `claimed_user_id:claimed_device_id` but whose leaf is signed by a fresh,
/// uncertified key, as a malicious DS would hand a committer. It passes
/// `KeyPackageIn::validate`, so only the cross-signing check can reject it.
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
    build_one(state, user_id, device_id, suite, false).await
}

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

/// Rotate this device's key packages on every login: replace its unclaimed
/// pool and last-resort package with fresh ones.
///
/// The last-resort package rides in the same request as the delete, so there is
/// no instant at which the device has neither (#1571).
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
    let mut all_packages = kp_packages_json(&pairs, current_suite());
    let last_resort = build_one(state, user_id, device_id, current_suite(), true).await?;
    all_packages.push(last_resort_json(&last_resort, current_suite()));

    // One DS transaction clears the stale unclaimed pool and inserts the fresh one.
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
/// `reported` is the DS's unclaimed count from the drain request; when present
/// it is the whole answer, because packages leave the pool without a Welcome (a
/// lost-race claim, a refused leaf). `None` (an older DS) falls back to refill
/// after a Welcome rather than a count read on every drain.
/// `has_last_resort == Some(false)` always refills (#1571).
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

/// Top up this device's pool to [`POOL_TARGET`] without deleting existing ones.
///
/// Single-flight per process: concurrent drains see the same low count, and
/// publishing from each would overfill by the shortfall once per drain; the
/// running one re-counts and fills it all.
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
    // Current suite only: a retired suite's unclaimable packages would mask an
    // empty live pool. Scoped by the DS to the authenticated device, not a
    // body-named one. `has_last_resort == None` is an older DS, which would
    // store one as a regular package, so none is sent.
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

    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(needed as usize);
    for _ in 0..needed {
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let mut all_packages = kp_packages_json(&pairs, current_suite());
    if wants_last_resort {
        let last_resort = build_one(state, user_id, device_id, current_suite(), true).await?;
        all_packages.push(last_resort_json(&last_resort, current_suite()));
    }

    let body = pollis_api::devices::PublishKeyPackagesBody {
        device_id: device_id.to_string(),
        packages: all_packages,
        user_id: Some(user_id.to_string()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Validate a claimed `KeyPackage` blob against the expected user's credential,
/// returning its hex-encoded `KeyPackageRef`.
///
/// Generic over the crypto backend because the suite is inside the blob, so an
/// unsupported suite stays a runtime error. Callers pass `provider.crypto()`.
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

    #[test]
    fn a_reported_short_pool_is_refilled_without_a_welcome() {
        assert!(drain_wants_refill(false, Some(0), Some(true)));
        assert!(drain_wants_refill(false, Some(POOL_TARGET - 1), Some(true)));
    }

    #[test]
    fn a_missing_last_resort_package_is_published_on_the_next_drain() {
        assert!(drain_wants_refill(false, Some(POOL_TARGET), Some(false)));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET), None));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET), Some(true)));
    }

    #[test]
    fn a_reported_full_pool_is_not_refilled() {
        assert!(!drain_wants_refill(true, Some(POOL_TARGET), Some(true)));
        assert!(!drain_wants_refill(false, Some(POOL_TARGET + 3), Some(true)));
    }

    #[test]
    fn without_a_count_only_a_processed_welcome_refills() {
        assert!(drain_wants_refill(true, None, None));
        assert!(!drain_wants_refill(false, None, None));
    }
}
