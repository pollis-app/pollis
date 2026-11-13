//! Leaf certification at the DS (#1851): no MLS object the DS stores or hands
//! out may put a leaf in a group unless the claimed account certified that
//! leaf's device key.
//!
//! A leaf is **certified** when, on the main DB, its `user_id:device_id`
//! credential names a live (`revoked_at IS NULL`) `user_device` row whose
//! `device_cert` verifies under that user's current `users.account_id_pub`
//! ([`crate::cert::verify_device_cert`], the publish-time check), and the leaf's
//! signature key is byte-equal to the certified key for the group's signature
//! scheme. It is the same rule as pollis-core's
//! `IdentityDirectory::leaf_verdict`, except that the DS refuses what the client
//! would call unverifiable (no row, no cert yet): the DS has the rows, so
//! "cannot be shown certified" is a refusal here.
//!
//! What the DS reads, and where ([`crate::devices`], [`crate::commit`],
//! [`crate::writes`]):
//!
//! | Object | Endpoint | Check |
//! |---|---|---|
//! | KeyPackage | `/v1/key-packages`, `/v1/key-packages/replenish` | signature valid, credential is the actor's device, `ref_hash` is its KeyPackageRef, leaf certified |
//! | KeyPackage | `/v1/key-packages/claim` | leaf still certified (revocation, identity reset); otherwise burnt, never served |
//! | Welcome | `/v1/commits`, `/v1/welcomes/resubmit` | every new member's KeyPackageRef is a stored package of a certified device, one of them the recipient's |
//! | GroupInfo | `/v1/commits`, `/v1/group-info` | every leaf in its ratchet tree that the stored GroupInfo lacks is certified |
//!
//! Commits themselves are opaque: a member commit is a `PrivateMessage`, and
//! the DS holds no group secrets. It sees an add through the Welcome and the
//! resulting GroupInfo that ride with the commit. Members check every leaf a
//! commit grafts from the commit itself (whitepaper §5.3).
//!
//! Parsing only: the DS keeps no MLS group state and holds no MLS key.

use std::collections::HashSet;

use libsql::Connection;
use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::{
    Ciphersuite, Credential, CredentialType, KeyPackageIn, MlsMessageBodyIn, MlsMessageIn,
    ProtocolVersion, SignatureScheme,
};
use openmls::prelude::group_info::VerifiableGroupInfo;
use openmls_rust_crypto::RustCrypto;

/// Whether a write path runs the leaf checks.
///
/// Production always enforces. [`LeafCheck::Skip`] exists so the integration
/// harness can model a DS that does not (whitepaper §1: the DS is untrusted)
/// and prove the client-side checks still hold on their own, and so tests of
/// the other submit gates can use placeholder MLS bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafCheck {
    Enforce,
    Skip,
}

/// One leaf, as the DS reads it off a KeyPackage or a ratchet tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Leaf {
    pub user_id: String,
    pub device_id: String,
    pub signature_key: Vec<u8>,
    /// From the object's ciphersuite: picks which certified device key the
    /// leaf's signature key has to equal.
    pub scheme: SignatureScheme,
}

/// Why a leaf or an object carrying leaves was refused. `&'static str` so no
/// caller input is echoed back.
pub type Refusal = &'static str;

/// `user_id:device_id`, as pollis-core's `make_credential` writes it. A legacy
/// user-only credential names no device and so can never be certified.
fn credential_ids(credential: &Credential) -> Option<(String, String)> {
    if credential.credential_type() != CredentialType::Basic {
        return None;
    }
    let s = std::str::from_utf8(credential.serialized_content()).ok()?;
    let (user_id, device_id) = s.split_once(':')?;
    if user_id.is_empty() || device_id.is_empty() {
        return None;
    }
    Some((user_id.to_string(), device_id.to_string()))
}

fn leaf_of(credential: &Credential, signature_key: &[u8], suite: Ciphersuite) -> Option<Leaf> {
    let (user_id, device_id) = credential_ids(credential)?;
    Some(Leaf {
        user_id,
        device_id,
        signature_key: signature_key.to_vec(),
        scheme: suite.signature_algorithm(),
    })
}

/// A published KeyPackage, read and checked for self-consistency.
#[derive(Debug, Clone)]
pub struct InspectedKeyPackage {
    pub leaf: Leaf,
    /// Hex KeyPackageRef, the value clients file the package under.
    pub ref_hash: String,
    /// The package's MLS ciphersuite code point.
    pub suite: i64,
    pub last_resort: bool,
}

/// Parse a KeyPackage and verify its own signatures (RFC 9420 §10.1). The
/// lifetime check is openmls's; a freshly built package always passes it.
pub fn inspect_key_package(bytes: &[u8]) -> Result<InspectedKeyPackage, Refusal> {
    let mut reader = bytes;
    let kp_in = KeyPackageIn::tls_deserialize(&mut reader).map_err(|_| "key package does not parse")?;
    if !reader.is_empty() {
        return Err("key package has trailing bytes");
    }
    let crypto = RustCrypto::default();
    let kp = kp_in
        .validate(&crypto, ProtocolVersion::Mls10)
        .map_err(|_| "key package does not validate")?;
    let suite = kp.ciphersuite();
    let leaf_node = kp.leaf_node();
    let leaf = leaf_of(leaf_node.credential(), leaf_node.signature_key().as_slice(), suite)
        .ok_or("key package credential names no device")?;
    let ref_hash = kp
        .hash_ref(&crypto)
        .map(|r| hex::encode(r.as_slice()))
        .map_err(|_| "key package reference cannot be computed")?;
    Ok(InspectedKeyPackage {
        leaf,
        ref_hash,
        suite: i64::from(u16::from(suite)),
        last_resort: kp.last_resort(),
    })
}

/// The leaf a STORED KeyPackage carries, without re-verifying its signature
/// or lifetime: it was verified when it was published (or, for a package
/// stored before #1851, the committer verifies it on claim). Used at claim
/// time, where only the certification can have changed since.
pub fn stored_key_package_leaf(bytes: &[u8]) -> Option<Leaf> {
    let mut reader = bytes;
    let kp_in = KeyPackageIn::tls_deserialize(&mut reader).ok()?;
    let cwk = kp_in.unverified_credential();
    // The suite is not reachable on an unverified package; read it off the
    // wire instead: `version (u16) || cipher_suite (u16) || …`.
    let suite_code = u16::from_be_bytes([*bytes.get(2)?, *bytes.get(3)?]);
    let suite = Ciphersuite::try_from(suite_code).ok()?;
    leaf_of(&cwk.credential, cwk.signature_key.as_slice(), suite)
}

/// Every new member a Welcome admits, by hex KeyPackageRef.
pub fn welcome_new_members(bytes: &[u8]) -> Result<Vec<String>, Refusal> {
    let mut reader = bytes;
    let msg = MlsMessageIn::tls_deserialize(&mut reader).map_err(|_| "welcome does not parse")?;
    match msg.extract() {
        MlsMessageBodyIn::Welcome(welcome) => Ok(welcome
            .secrets()
            .iter()
            .map(|s| hex::encode(s.new_member().as_slice()))
            .collect()),
        _ => Err("welcome is not a Welcome"),
    }
}

/// A stored or submitted GroupInfo, in either wire form clients have written:
/// the `MlsMessage` envelope, or (rows written before it) a bare GroupInfo.
fn parse_group_info(bytes: &[u8]) -> Option<VerifiableGroupInfo> {
    let mut reader = bytes;
    if let Ok(msg) = MlsMessageIn::tls_deserialize(&mut reader) {
        return match msg.extract() {
            MlsMessageBodyIn::GroupInfo(gi) => Some(gi),
            _ => None,
        };
    }
    let mut reader = bytes;
    VerifiableGroupInfo::tls_deserialize(&mut reader).ok()
}

/// Every leaf in a GroupInfo's ratchet tree. `Err` when the GroupInfo does not
/// parse, carries no ratchet tree, or has a leaf whose credential names no
/// device: none of those can be checked, so none is stored.
pub fn group_info_leaves(bytes: &[u8]) -> Result<HashSet<Leaf>, Refusal> {
    let gi = parse_group_info(bytes).ok_or("group info does not parse")?;
    let suite = gi.group_context().ciphersuite();
    let tree = gi
        .extensions()
        .ratchet_tree()
        .ok_or("group info carries no ratchet tree")?;
    tree.ratchet_tree()
        .leaves()
        .map(|leaf| {
            leaf_of(leaf.credential(), leaf.signature_key().as_slice(), suite)
                .ok_or("group info has a leaf whose credential names no device")
        })
        .collect()
}

/// Is `leaf` a device its account certified, per the main DB right now?
pub async fn leaf_verdict(main_conn: &Connection, leaf: &Leaf) -> anyhow::Result<Result<(), Refusal>> {
    let mut rows = main_conn
        .query(
            "SELECT u.account_id_pub, d.device_cert, d.cert_issued_at, d.cert_identity_version, \
                    d.mls_signature_pub, d.mls_signature_pub_pq, d.revoked_at \
             FROM user_device d JOIN users u ON u.id = d.user_id \
             WHERE d.device_id = ?1 AND d.user_id = ?2",
            libsql::params![leaf.device_id.clone(), leaf.user_id.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(Err("leaf device is not registered to that account"));
    };
    if row.get::<Option<String>>(6)?.is_some() {
        return Ok(Err("leaf device is revoked"));
    }
    let (Some(account_pub), Some(cert), Some(issued_at), Some(version), Some(ed_pub), Some(pq_pub)) = (
        row.get::<Option<Vec<u8>>>(0)?,
        row.get::<Option<Vec<u8>>>(1)?,
        row.get::<Option<String>>(2)?,
        row.get::<Option<i64>>(3)?,
        row.get::<Option<Vec<u8>>>(4)?,
        row.get::<Option<Vec<u8>>>(5)?,
    ) else {
        return Ok(Err("leaf device has no device cert"));
    };
    let (Ok(issued_at), Ok(version)) = (issued_at.parse::<u64>(), u32::try_from(version)) else {
        return Ok(Err("leaf device cert is malformed"));
    };
    if !crate::cert::verify_device_cert(&account_pub, &leaf.device_id, &ed_pub, &pq_pub, version, issued_at, &cert) {
        return Ok(Err("leaf device cert does not verify under the account key"));
    }
    let certified: &[u8] = match leaf.scheme {
        SignatureScheme::ED25519 => &ed_pub,
        _ => &pq_pub,
    };
    if certified != leaf.signature_key.as_slice() {
        return Ok(Err("leaf signature key is not the certified device key"));
    }
    Ok(Ok(()))
}

/// [`leaf_verdict`] over a set, stopping at the first refusal.
pub async fn all_certified<'a>(
    main_conn: &Connection,
    leaves: impl IntoIterator<Item = &'a Leaf>,
) -> anyhow::Result<Result<(), Refusal>> {
    for leaf in leaves {
        if let Err(why) = leaf_verdict(main_conn, leaf).await? {
            return Ok(Err(why));
        }
    }
    Ok(Ok(()))
}

/// A KeyPackage the actor publishes for `device_id` ([`inspect_key_package`]
/// already validated it): it must be filed under its own KeyPackageRef and
/// suite, carry the actor's own `user:device` credential, and be certified.
pub async fn check_published_key_package(
    main_conn: &Connection,
    actor: &str,
    device_id: &str,
    declared_ref: &str,
    declared_suite: i64,
    kp: &InspectedKeyPackage,
) -> anyhow::Result<Result<(), Refusal>> {
    if !kp.ref_hash.eq_ignore_ascii_case(declared_ref) {
        return Ok(Err("ref_hash is not the key package's reference"));
    }
    if kp.suite != declared_suite {
        return Ok(Err("ciphersuite is not the key package's suite"));
    }
    if kp.leaf.user_id != actor || kp.leaf.device_id != device_id {
        return Ok(Err("key package credential is not the publishing device"));
    }
    leaf_verdict(main_conn, &kp.leaf).await
}

/// The Welcomes of one submit or resubmit: each new member must be a
/// certified device's stored KeyPackage, and the recipient must be among them.
pub async fn check_welcome(
    main_conn: &Connection,
    recipient_id: &str,
    recipient_device_id: &str,
    welcome: &[u8],
) -> anyhow::Result<Result<(), Refusal>> {
    let refs = match welcome_new_members(welcome) {
        Ok(refs) => refs,
        Err(why) => return Ok(Err(why)),
    };
    let mut recipient_admitted = false;
    for ref_hash in refs {
        let mut rows = main_conn
            .query(
                "SELECT user_id, device_id, key_package FROM mls_key_package WHERE ref_hash = ?1",
                libsql::params![ref_hash.to_ascii_lowercase()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(Err("welcome admits a key package the DS never accepted"));
        };
        let user_id: String = row.get(0)?;
        let device_id: Option<String> = row.get(1)?;
        let bytes: Vec<u8> = row.get(2)?;
        drop(rows);
        let Some(leaf) = stored_key_package_leaf(&bytes) else {
            return Ok(Err("welcome admits a key package the DS cannot read"));
        };
        if leaf.user_id != user_id || device_id.as_deref() != Some(leaf.device_id.as_str()) {
            return Ok(Err("welcome admits a key package filed under another device"));
        }
        if let Err(why) = leaf_verdict(main_conn, &leaf).await? {
            return Ok(Err(why));
        }
        if leaf.user_id == recipient_id && leaf.device_id == recipient_device_id {
            recipient_admitted = true;
        }
    }
    if !recipient_admitted {
        return Ok(Err("welcome does not admit its recipient device"));
    }
    Ok(Ok(()))
}

/// A GroupInfo about to replace `stored` (the conversation's current row, if
/// any): every leaf it has that `stored` lacks must be certified.
///
/// Diffed against the stored GroupInfo rather than the previous epoch's, so a
/// leaf that arrived in a commit published without a GroupInfo is still
/// checked at the next one. A stored GroupInfo the DS cannot read counts as
/// empty: every leaf is then checked.
pub async fn check_group_info(
    main_conn: &Connection,
    stored: Option<&[u8]>,
    group_info: &[u8],
) -> anyhow::Result<Result<(), Refusal>> {
    let leaves = match group_info_leaves(group_info) {
        Ok(leaves) => leaves,
        Err(why) => return Ok(Err(why)),
    };
    let before: HashSet<Leaf> = stored
        .and_then(|b| group_info_leaves(b).ok())
        .unwrap_or_default();
    all_certified(main_conn, leaves.difference(&before)).await
}

/// The conversation's stored GroupInfo blob, the baseline [`check_group_info`]
/// diffs against.
pub async fn stored_group_info(log_conn: &Connection, conversation_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let mut rows = log_conn
        .query(
            "SELECT group_info FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<Vec<u8>>>(0)?,
        None => None,
    })
}
