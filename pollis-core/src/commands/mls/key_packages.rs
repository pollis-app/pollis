//! MLS key-package lifecycle.
//!
//! The KeyPackage pool (build + publish + replenish) that precedes any MLS
//! group operation. `ensure_mls_key_package` rotates this device's pool on
//! login; `replenish_key_packages` tops it up whenever the Welcome drain reports
//! the pool is low (#1412).
//! Both route owner-scoped writes through the Delivery Service.
//!
//! ## The last-resort package (#1571)
//!
//! Beside the [`POOL_TARGET`] one-shot packages, a device publishes ONE
//! last-resort package per suite (RFC 9420 §16.8, `mark_as_last_resort`). The
//! Delivery Service hands it out only when the device's pool is empty, and never
//! marks it claimed, so "a registered device of a roster member has nothing to
//! be added with" — which left a phone silently outside a DM's tree — cannot
//! happen to a device running this build. openmls keeps a last-resort package's
//! private key in storage after a join (`keys_for_welcome` skips its delete),
//! and nothing in `signal::mls_storage` deletes key packages on its own, so the
//! same package admits the device to any number of groups. Reuse is the
//! standard trade: the Welcomes sealed to it share an init key until the
//! post-join self-update (#666) rotates the leaf.
//!
//! A login rotation replaces it with a fresh one in the same request. A top-up
//! publishes one only when the Delivery Service says the device has none and
//! knows what one is (`has_last_resort == Some(false)`).

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
/// The package's leaf is signed with the device's stable key **for `suite`'s
/// signature scheme** (#668) — ML-DSA-44 for [`CS_PQ`](super::provider::CS_PQ). That key is stable
/// across rotations, so a device's whole pool keeps presenting the same signing
/// key. `suite` stays a parameter rather than reading `CS_PQ` directly so that a
/// code-point change needs no edit here.
///
/// Sync: the caller owns the local-DB guard. A regular package; the unit tests'
/// shorthand for [`build_key_package`].
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
        // RFC 9420 §7.2: every extension a KeyPackage carries must be listed in
        // its leaf's capabilities, and `KeyPackageIn::validate` — which every
        // committer runs on a claimed package, shipped builds included — refuses
        // the package otherwise (`UnsupportedExtension`). Only the extension
        // list moves off the defaults, so the leaf advertises exactly what a
        // regular package's does plus `LastResort`.
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

/// Test-harness only: a KeyPackage in the current suite whose credential claims
/// `claimed_user_id:claimed_device_id` but whose leaf is signed by a FRESH key
/// nobody's account ever certified. This is the KeyPackage a malicious Delivery
/// Service would hand a committer at claim time in place of the real device's;
/// `KeyPackageIn::validate` accepts it (it is self-consistent) and the credential
/// string matches, so only the cross-signing check can tell it apart.
///
/// The forged signer is not stored anywhere — a device that could not sign as
/// this leaf later is the point: the test only needs the package bytes.
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

/// Build one fresh `KeyPackage` for this device in `suite`, backed by the current
/// local DB. Locks the local DB for the (sync) openmls work, dropping the guard
/// before returning.
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
/// Called from `initialize_identity` on every login.  Only deletes packages
/// for the current `device_id` — other devices' packages are left intact.
///
/// The last-resort package rides in the SAME replenish request, so the rotation
/// that deletes the old one (its private key may be gone with a wiped local DB)
/// is the one that publishes its replacement: there is no instant at which the
/// device has neither. A Delivery Service that predates #1571 stores it as a
/// sixth regular package, which costs nothing.
pub async fn ensure_mls_key_package(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<()> {
    const TARGET: i64 = POOL_TARGET;

    // Build TARGET fresh packages locally (their private keys live in the local
    // DB) before any remote write, so the DS replenish is a single batched call.
    let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(TARGET as usize);
    for _ in 0..TARGET {
        pairs.push(build_one_key_package_in_suite(state, user_id, device_id, current_suite()).await?);
    }
    let mut all_packages = kp_packages_json(&pairs, current_suite());
    let last_resort = build_one(state, user_id, device_id, current_suite(), true).await?;
    all_packages.push(last_resort_json(&last_resort, current_suite()));

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

/// Should a Welcome drain top this device's pool up (#1412)?
///
/// `reported` is the device's unclaimed count as the Delivery Service read it
/// in the same request as the drain. When it is there, it is the whole answer:
/// refill whenever the pool is below [`POOL_TARGET`], Welcome or no Welcome.
/// Packages leave the pool without a Welcome ever reaching this device — a
/// peer's claim whose commit lost its epoch race, a leaf the committer refused —
/// and a refill keyed only on Welcomes let exactly those drain a backgrounded
/// device to zero, after which every new conversation skipped it.
///
/// `None` is a Delivery Service that predates the count: keep the old rule
/// (refill after processing a Welcome) rather than pay a separate count read on
/// every drain, which runs on every wake-up and every conversation opened.
///
/// `has_last_resort` is the same drain's answer to "does this device hold a
/// last-resort package" (#1571). `Some(false)` refills too, whatever the count:
/// a device that upgraded from a build without one, or whose package was lost,
/// gets one on its next wake-up rather than at its next sign-in. `None` (a server
/// that predates it) changes nothing.
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

/// Top-up key packages for this device to [`POOL_TARGET`] without deleting
/// existing ones. Called from the Welcome drain whenever the pool is low
/// ([`drain_wants_refill`]), so the device stays addable to new conversations.
///
/// Single-flight per process: a foreground pass ingests every conversation at
/// once and each ingest drains Welcomes, so several drains can see the same low
/// count together. Only one tops up; the others return, because the one that is
/// running re-counts and fills the whole shortfall — publishing from each would
/// overfill the pool by the shortfall once per concurrent drain.
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
    // Count only packages in the CURRENT suite. A package left behind by a
    // retired suite is unclaimable — counting it would let a dead pool mask an
    // empty live one and silently make this device unaddable.
    //
    // Counted by the DS since #987, and scoped to the AUTHENTICATED device
    // rather than to a body-named one — this number drives how many packages
    // this device publishes, and counting someone else's pool would make it
    // publish into the wrong one.
    //
    // The same read says whether the device holds a last-resort package
    // (#1571): `Some(false)` publishes one alongside the top-up. `None` is a
    // server that predates them, which would store one as a regular package —
    // so nothing is sent.
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

#[cfg(test)]
mod tests {
    use super::{drain_wants_refill, POOL_TARGET};

    /// #1412: packages leave the pool with no Welcome (a lost-race claim, a
    /// refused leaf). When the DS reports the count, a short pool is refilled
    /// on any drain, Welcome or not.
    #[test]
    fn a_reported_short_pool_is_refilled_without_a_welcome() {
        assert!(drain_wants_refill(false, Some(0), Some(true)));
        assert!(drain_wants_refill(false, Some(POOL_TARGET - 1), Some(true)));
    }

    /// #1571: a device with a full pool but no last-resort package (an upgrade
    /// from a build without one) refills on its next drain. A server that does
    /// not say (`None`) changes nothing.
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

    /// An older DS reports nothing: fall back to the pre-#1412 rule rather than
    /// paying a separate count read on every drain.
    #[test]
    fn without_a_count_only_a_processed_welcome_refills() {
        assert!(drain_wants_refill(true, None, None));
        assert!(!drain_wants_refill(false, None, None));
    }
}
