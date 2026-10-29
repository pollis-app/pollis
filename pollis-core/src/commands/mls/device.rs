//! Per-device stable MLS signing keys and cross-signing certificates.
//!
//! Each device has a single stable MLS signing keypair (so every KeyPackage
//! it ever ships is covered by one `device_cert` in `user_device`). The
//! cert chains the device's signing pub to the user's `account_id_pub`,
//! and is re-signed whenever the account identity rotates.

use openmls::prelude::SignatureScheme;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;

use crate::state::AppState;

use super::provider::{MlsProvider, PollisProvider};

/// Custom scope in `mls_kv` that stores the stable per-device MLS
/// signature public-key bytes. The private side is held by openmls under
/// its own `SignatureKeyPair` scope, looked up by these same bytes.
const DEVICE_SIG_PUB_SCOPE: &str = "PollisDeviceSigPub";

/// The `mls_kv` key holding this device's stable signing pub for one signature
/// scheme.
fn device_sig_pub_kv_key(user_id: &str, device_id: &str, scheme: SignatureScheme) -> Vec<u8> {
    format!("{user_id}:{device_id}:{}", scheme as u16).into_bytes()
}

/// The pre-#668 `mls_kv` key: no scheme suffix, always Ed25519.
///
/// Read-only: every shipped device's Ed25519 pub lives here; dropping it would strand its groups.
fn legacy_device_sig_pub_kv_key(user_id: &str, device_id: &str) -> Vec<u8> {
    format!("{user_id}:{device_id}").into_bytes()
}

fn read_kv(
    conn: &rusqlite::Connection,
    key: &[u8],
) -> crate::error::Result<Option<Vec<u8>>> {
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

fn load_stable_device_sig_pub_bytes(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
    scheme: SignatureScheme,
) -> crate::error::Result<Option<Vec<u8>>> {
    if let Some(bytes) = read_kv(conn, &device_sig_pub_kv_key(user_id, device_id, scheme))? {
        return Ok(Some(bytes));
    }
    if scheme == SignatureScheme::ED25519 {
        return read_kv(conn, &legacy_device_sig_pub_kv_key(user_id, device_id));
    }
    Ok(None)
}

fn store_stable_device_sig_pub_bytes(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
    scheme: SignatureScheme,
    pub_bytes: &[u8],
) -> crate::error::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            DEVICE_SIG_PUB_SCOPE,
            device_sig_pub_kv_key(user_id, device_id, scheme),
            pub_bytes
        ],
    )?;
    // Keep the un-suffixed row in step for Ed25519 so a downgrade to a v1.7.0
    // build still finds the key its groups are signed with.
    if scheme == SignatureScheme::ED25519 {
        conn.execute(
            "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                DEVICE_SIG_PUB_SCOPE,
                legacy_device_sig_pub_kv_key(user_id, device_id),
                pub_bytes
            ],
        )?;
    }
    Ok(())
}

/// Return the stable MLS signing keypair for this device, creating it if
/// missing. All key packages and group creation on this device MUST use
/// this keypair so the device-level cross-signing cert in `user_device`
/// covers every leaf node this device produces.
///
/// Returns `(SignatureKeyPair, pub_bytes)`. The Ed25519 pub_bytes are also what
/// gets signed into the `device_cert` in `user_device`.
///
/// Stable per signature scheme (#668): callers pass the scheme of the suite they operate in.
pub fn load_or_create_device_signer<C>(
    provider: &MlsProvider<'_, C>,
    user_id: &str,
    device_id: &str,
    scheme: SignatureScheme,
) -> crate::error::Result<(SignatureKeyPair, Vec<u8>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    // Fast path: pub bytes are stashed → recover the private side from
    // openmls storage and return.
    if let Some(pub_bytes) = load_stable_device_sig_pub_bytes(
        provider.raw_conn(),
        user_id,
        device_id,
        scheme,
    )? {
        if let Some(kp) = SignatureKeyPair::read(provider.storage(), &pub_bytes, scheme) {
            return Ok((kp, pub_bytes));
        }
        // Pub bytes stashed but the private side is gone (e.g. mls_kv
        // got partially wiped). Fall through to regenerate.
        crate::diag!(
            "[mls] stable device signer pub present but private missing for {user_id}:{device_id} \
             ({scheme:?}) — regenerating"
        );
    }

    // Slow path: create, store, stash.
    let sig_keys = SignatureKeyPair::new(scheme)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;
    sig_keys
        .store(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;
    let pub_bytes = sig_keys.to_public_vec();
    store_stable_device_sig_pub_bytes(
        provider.raw_conn(),
        user_id,
        device_id,
        scheme,
        &pub_bytes,
    )?;
    Ok((sig_keys, pub_bytes))
}

/// Load this device's stable ML-DSA-44 MLS signing key as an
/// `ml_dsa::SigningKey<MlDsa44>` — the PQ suite's leaf key, and (since #668 P6)
/// the key the relay handshake's possession signature is made with.
///
/// openmls stores the ML-DSA key as its 32-byte seed, so seed recovery works as for Ed25519.
pub fn load_device_pq_signing_key(
    provider: &PollisProvider<'_>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<(ml_dsa::SigningKey<ml_dsa::MlDsa44>, Vec<u8>)> {
    let (kp, pub_bytes) =
        load_or_create_device_signer(provider, user_id, device_id, SignatureScheme::MLDSA44)?;
    let seed: [u8; 32] = kp.private().try_into().map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!(
            "device ML-DSA signing key is not a 32-byte seed"
        ))
    })?;
    Ok((
        ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&seed.into()),
        pub_bytes,
    ))
}

/// Both of this device's certified leaf public keys, `(ed25519, ml_dsa_44)`,
/// creating either if missing.
///
/// A device cert covers BOTH (#668 P4), so neither leaf is uncertified.
pub fn load_device_cert_pubs(
    provider: &PollisProvider<'_>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<(Vec<u8>, Vec<u8>)> {
    let (_, ed_pub) =
        load_or_create_device_signer(provider, user_id, device_id, SignatureScheme::ED25519)?;
    let (_, pq_pub) =
        load_or_create_device_signer(provider, user_id, device_id, SignatureScheme::MLDSA44)?;
    Ok((ed_pub, pq_pub))
}

/// Ensure this device has a stable MLS signing keypair AND a `device_cert`
/// published in `user_device` binding the pub bytes to the user's
/// `account_id_key`. Idempotent — safe to call on every login.
///
/// Skipped (returns `false`) when `account_id_key` is not in the local keystore.
pub async fn ensure_device_cert(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<bool> {
    // 0. Bail early if we don't have the account identity locally. This
    //    happens on a new device before step-5 enrollment has run.
    if !crate::commands::account_identity::has_local_account_identity(
        state.as_ref(),
        user_id,
    ).await? {
        return Ok(false);
    }

    // 1. Load or create BOTH signing keypairs; cert v2 (#668) binds both.
    let (sig_pub_bytes, pq_sig_pub_bytes) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        load_device_cert_pubs(&provider, user_id, device_id)?
    };

    // 2. Read the current identity_version to stamp into the cert.
    //   Via the UNAUTHENTICATED probe: `mls_signature_pub` is still NULL here, so a signed read would 401.
    let identity_version: u32 = crate::commands::ds_reads::account_probe(state, user_id)
        .await?
        .identity_version
        .ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!(
                "user {user_id} has no account identity while signing device cert"
            ))
        })? as u32;

    // 3. Sign the cert with the account identity key loaded from the OS
    //    keystore, using the current unix time as `issued_at`.
    let issued_at = crate::util::now_unix();

    let cert = crate::commands::account_identity::sign_device_cert(
        state.as_ref(),
        user_id,
        device_id,
        &sig_pub_bytes,
        &pq_sig_pub_bytes,
        identity_version,
        issued_at,
    )
    .await?;

    // 4. Write cert + keys into the remote `user_device` row.

    // BOOTSTRAP PIVOT: this write sets `mls_signature_pub`, the column the DS authenticates with, so it cannot be signed. Two paths:
    //
    //   * First device: OTP session + cert-validity; the token is single-use.
    //   * Later device: cert-validity ALONE, which proves the account key and outlives the session TTL.
    //
    // See `docs/otp-server-bootstrap-design.md` §5.
    let bootstrap_session = state.bootstrap_session.lock().await.clone();
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let body = pollis_api::bootstrap::PublishCertBody {
        device_id: device_id.to_string(),
        device_cert: b64.encode(&cert),
        cert_issued_at: issued_at as i64,
        cert_identity_version: identity_version,
        mls_signature_pub: b64.encode(&sig_pub_bytes),
        mls_signature_pub_pq: b64.encode(&pq_sig_pub_bytes),
        user_id: Some(user_id.to_string()),
    };
    match bootstrap_session {
        Some(token) => {
            // First-device signup: session + cert-validity, single-use.
            crate::commands::mls::ds_post_session_ok(
                state,
                &token,
                &body,
            )
            .await?;
            // Spent server-side; the next call takes the cert-validity path.
            *state.bootstrap_session.lock().await = None;
        }
        None => {
            // Subsequent device: cert-validity ALONE — no session header.
            let resp = crate::commands::mls::ds_post_plain(
                state,
                &body,
            )
            .await?;
            if !resp.status().is_success() {
                let s = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "publish-device-cert (cert-validity) {s}: {txt}"
                )));
            }
            // The first cert publish completes this device's bootstrap.
            *state.enrollment_session.lock().await = None;
        }
    }

    crate::diag!(
        "[mls] device cert published for {user_id}:{device_id} (identity_version={identity_version})"
    );

    Ok(true)
}

/// One device that needs its cross-signing cert re-signed: its id and both leaf
/// signing keys, decoded from the base64 the DS serves.
///
/// A named type: both keys are the same shape, and a swap would mint a wrong cert.
pub struct StaleCertDevice {
    pub device_id: String,
    /// Ed25519 leaf signing key (classic MLS suite).
    pub mls_signature_pub: Vec<u8>,
    /// ML-DSA-44 leaf signing key (PQ suite, #668).
    pub mls_signature_pub_pq: Vec<u8>,
}

/// The stale-cert candidate set for `user_id` at account identity version
/// `identity_version`: every `user_device` row that still needs re-signing.
///
/// "Stale": `cert_identity_version` NULL or below `identity_version`.
///
/// Excluded:
///   - **revoked** rows (#685), never re-signed;
///   - rows with a NULL leaf pub, which certify when next online.
///
/// Revoked rows are filtered here too, not only by the DS query.
pub fn stale_cert_candidates(
    rows: &[pollis_api::account_reads::DeviceRow],
    identity_version: i64,
) -> crate::error::Result<Vec<StaleCertDevice>> {
    let mut out = Vec::new();
    for row in rows {
        if row.revoked_at.is_some() {
            continue;
        }
        // No leaf pub yet: certified when it next comes online.
        let (Some(sig), Some(pq)) = (
            row.mls_signature_pub.as_deref(),
            row.mls_signature_pub_pq.as_deref(),
        ) else {
            continue;
        };
        // NULL `cert_identity_version` means "never certified", which is stale
        // by definition — the same reading the SQL's `IS NULL OR <` gave.
        let stale = row
            .cert_identity_version
            .is_none_or(|v| v < identity_version);
        if !stale {
            continue;
        }
        out.push(StaleCertDevice {
            device_id: row.device_id.clone(),
            mls_signature_pub: super::ds_reads::decode_b64("mls_signature_pub", sig)?,
            mls_signature_pub_pq: super::ds_reads::decode_b64("mls_signature_pub_pq", pq)?,
        });
    }
    Ok(out)
}

/// Re-sign every stale `user_device` row for `user_id` with the user's
/// current account identity key, stamping each row's `device_cert`,
/// `cert_issued_at`, and `cert_identity_version` to match
/// `users.identity_version`. Staleness (and the revoked / NULL-leaf-pub
/// exclusions) is defined by [`stale_cert_candidates`].
///
/// Called after an identity rotation and on unlock, so a fleet self-heals as users come online.
///
/// Returns the number of rows re-signed.
pub async fn resign_stale_device_certs(
    state: &Arc<AppState>,
    user_id: &str,
) -> crate::error::Result<usize> {
    let identity_version: u32 = crate::commands::ds_reads::account_status(state, user_id)
        .await?
        .map(|a| a.identity_version as u32)
        .ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!(
                "user {user_id} not found while re-signing device certs"
            ))
        })?;

    // The caller's own non-revoked devices, with cert columns.
    let rows = crate::commands::ds_reads::devices(state, Some(user_id), Vec::new(), false).await?;
    let devices = stale_cert_candidates(&rows, identity_version as i64)?;

    // Sign every cert BEFORE any remote write; `mls_signature_pub` is never touched.
    let mut signed: Vec<(String, Vec<u8>, String)> = Vec::with_capacity(devices.len());
    for StaleCertDevice {
        device_id,
        mls_signature_pub: sig_pub_bytes,
        mls_signature_pub_pq: pq_sig_pub_bytes,
    } in devices
    {
        let issued_at = crate::util::now_unix();

        let cert = crate::commands::account_identity::sign_device_cert(
            state.as_ref(),
            user_id,
            &device_id,
            &sig_pub_bytes,
            &pq_sig_pub_bytes,
            identity_version,
            issued_at,
        )
        .await?;
        signed.push((device_id, cert, issued_at.to_string()));
    }
    let count = signed.len();

    // User-scoped DS write: only the actor's own devices.
    if !signed.is_empty() {
        use base64::Engine as _;
        let certs: Vec<pollis_api::devices::ResignedCert> = signed
            .iter()
            .map(|(device_id, cert, issued_at_str)| pollis_api::devices::ResignedCert {
                device_id: device_id.clone(),
                device_cert: base64::engine::general_purpose::STANDARD.encode(cert),
                cert_issued_at: issued_at_str.clone(),
                cert_identity_version: identity_version as i64,
            })
            .collect();
        let body = pollis_api::devices::ResignDeviceCertsBody {
            certs,
            user_id: Some(user_id.to_string()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await?;
    }

    crate::diag!(
        "[mls] re-signed {count} device cert(s) for {user_id} at identity_version={identity_version}"
    );

    Ok(count)
}

/// One leaf an MLS commit grafts, as read off the MERGED TREE.
///
/// Diffed from the merged tree, never from the committer-written `added_*` prefetch hint.
///
/// The tree, not Add proposals: an external join's leaf arrives in the UpdatePath (#1161 H4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedLeaf {
    pub user_id: String,
    pub device_id: String,
    /// The leaf's signature key, which the device cert must certify.
    pub signature_key: Vec<u8>,
    /// The signature scheme of the KeyPackage's suite: decides which certified
    /// device key `signature_key` has to equal.
    pub scheme: SignatureScheme,
}

/// Verdict on one leaf (an added KeyPackage or a leaf already in the tree)
/// against the claimed user's cross-signing material.
///
/// Three-valued: both refusals block an ADD, but only `Uncertified` evicts an existing leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafVerdict {
    /// The leaf's signature key is the device key the user's `account_id_pub`
    /// certified, and the device is not revoked.
    Certified,
    /// The inputs to decide are missing: unknown user, no account key, no device
    /// row, or cert columns not yet published.
    Unverifiable(&'static str),
    /// The inputs are present and the leaf is NOT the user's: the device is
    /// revoked, the cert chain fails, or the leaf key is not the certified key.
    Uncertified(String),
}

/// The cross-signing material of a set of users, indexed for leaf verdicts.
///
/// Built from DS rows, but every verdict is computed here: the DS is outside the trust boundary.
pub struct IdentityDirectory {
    by_user: std::collections::HashMap<String, pollis_api::reads::AddedIdentity>,
    /// Whether [`Self::into_pinned`] has run, i.e. whether the two local
    /// substitutions — our own account key from the keystore, peers dropped
    /// where the DS disagrees with the TOFU pin — have been applied.
    ///
    /// Unpinned, it answers `Unverifiable` to everything (#1161 M3).
    pinned: bool,
}

impl IdentityDirectory {
    pub fn new(identities: Vec<pollis_api::reads::AddedIdentity>) -> Self {
        let by_user = identities
            .into_iter()
            .map(|i| (i.user_id.clone(), i))
            .collect();
        Self {
            by_user,
            pinned: false,
        }
    }

    /// Declare the local substitutions applied — see [`Self::pinned`].
    ///
    /// Called ONLY at the end of `reconcile::pin_identities`.
    pub(super) fn into_pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Replace one user's account key with a locally-trusted value, or drop it.
    ///
    /// Peers whose DS key differs from the TOFU pin get `None`; the actor's own key comes from the keystore.
    pub fn set_account_key(&mut self, user_id: &str, account_id_pub: Option<Vec<u8>>) {
        use base64::Engine as _;
        if let Some(identity) = self.by_user.get_mut(user_id) {
            identity.account_id_pub = account_id_pub
                .map(|k| base64::engine::general_purpose::STANDARD.encode(k));
        }
    }

    /// The DS-reported account key for `user_id`, decoded, if present.
    pub fn account_key(&self, user_id: &str) -> Option<Vec<u8>> {
        let identity = self.by_user.get(user_id)?;
        let encoded = identity.account_id_pub.as_deref()?;
        super::ds_reads::decode_b64("account_id_pub", encoded).ok()
    }

    /// Is the leaf `(user_id, device_id, signature_key)` the device the user's
    /// account key certified?
    ///
    /// `scheme` picks the certified key the leaf must equal; that equality stops a valid cert vouching for an attacker's leaf.
    pub fn leaf_verdict(
        &self,
        user_id: &str,
        device_id: &str,
        signature_key: &[u8],
        scheme: SignatureScheme,
    ) -> LeafVerdict {
        if !self.pinned {
            return LeafVerdict::Unverifiable("identity directory not pinned to local trust");
        }
        let Some(identity) = self.by_user.get(user_id) else {
            return LeafVerdict::Unverifiable("user not in directory");
        };
        let Some(account_id_pub) = identity
            .account_id_pub
            .as_deref()
            .and_then(|s| super::ds_reads::decode_b64("account_id_pub", s).ok())
        else {
            return LeafVerdict::Unverifiable("no account_id_pub");
        };
        let Some(row) = identity.devices.iter().find(|d| d.device_id == device_id) else {
            return LeafVerdict::Unverifiable("device not registered");
        };

        // Tombstone wins — a revoked device is unambiguously not allowed into a
        // tree, regardless of cert column state.
        if let Some(revoked_at) = &row.revoked_at {
            return LeafVerdict::Uncertified(format!("device revoked at {revoked_at}"));
        }

        let (cert, issued_at_str, cert_identity_version, ed_pub, pq_pub) = match (
            row.device_cert.as_deref(),
            row.cert_issued_at.as_deref(),
            row.cert_identity_version,
            row.mls_signature_pub.as_deref(),
            row.mls_signature_pub_pq.as_deref(),
        ) {
            (Some(c), Some(t), Some(v), Some(p), Some(q)) => {
                let decoded = (
                    super::ds_reads::decode_b64("device_cert", c),
                    super::ds_reads::decode_b64("mls_signature_pub", p),
                    super::ds_reads::decode_b64("mls_signature_pub_pq", q),
                );
                match decoded {
                    (Ok(c), Ok(p), Ok(q)) => (c, t, v, p, q),
                    _ => {
                        return LeafVerdict::Uncertified("cert columns not base64".to_string());
                    }
                }
            }
            // NULL cert columns on a live row: the cert publish has not landed.
            _ => {
                return LeafVerdict::Unverifiable("cert not published");
            }
        };

        let issued_at: u64 = match issued_at_str.parse() {
            Ok(v) => v,
            Err(e) => {
                return LeafVerdict::Uncertified(format!(
                    "cert_issued_at unparseable '{issued_at_str}': {e}"
                ));
            }
        };

        if let Err(e) = crate::commands::account_identity::verify_device_cert(
            &account_id_pub,
            device_id,
            &ed_pub,
            &pq_pub,
            cert_identity_version as u32,
            issued_at,
            &cert,
        ) {
            return LeafVerdict::Uncertified(format!("device cert invalid: {e}"));
        }

        // The cert is the user's and it certifies (ed_pub, pq_pub). The leaf has
        // to BE one of those keys — the one this group's suite signs with.
        let certified: &[u8] = match scheme {
            SignatureScheme::ED25519 => &ed_pub,
            _ => &pq_pub,
        };
        if certified != signature_key {
            return LeafVerdict::Uncertified(
                "leaf signature key is not the certified device key".to_string(),
            );
        }
        LeafVerdict::Certified
    }
}

#[cfg(test)]
mod leaf_verdict_tests {
    use super::*;
    use crate::commands::account_identity::{device_cert_signed_payload, AccountSigningKey};
    use ml_dsa::{Keypair as _, Signer as _};
    use pollis_api::reads::{AddedIdentity, DeviceCertRow};

    const DID: &str = "01DEVICE000000000000000000";
    const UID: &str = "01USER0000000000000000000";

    fn b64(b: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    /// A real account key, a real v2 cert over `(ed_pub, pq_pub)`, and the
    /// directory that describes them.
    fn certified_identity() -> (IdentityDirectory, Vec<u8>, Vec<u8>) {
        let account = AccountSigningKey::from_seed(&[42u8; 32].into());
        let ed_pub = vec![7u8; 32];
        let pq_pub = AccountSigningKey::from_seed(&[8u8; 32].into())
            .verifying_key()
            .encode()
            .to_vec();
        let payload =
            device_cert_signed_payload(DID, &ed_pub, &pq_pub, 1, 1_700_000_000).unwrap();
        let cert = account.sign(&payload).encode().to_vec();
        let dir = IdentityDirectory::new(vec![AddedIdentity {
            user_id: UID.to_string(),
            account_id_pub: Some(b64(&account.verifying_key().encode())),
            devices: vec![DeviceCertRow {
                device_id: DID.to_string(),
                device_cert: Some(b64(&cert)),
                cert_issued_at: Some("1700000000".to_string()),
                cert_identity_version: Some(1),
                mls_signature_pub: Some(b64(&ed_pub)),
                revoked_at: None,
                mls_signature_pub_pq: Some(b64(&pq_pub)),
            }],
        }])
        .into_pinned();
        (dir, ed_pub, pq_pub)
    }

    /// #1161 M3: a directory that has NOT been through the local substitutions
    /// cannot certify anything.
    #[test]
    fn an_unpinned_directory_certifies_nothing() {
        let (pinned, _ed_pub, pq_pub) = certified_identity();
        assert_eq!(
            pinned.leaf_verdict(UID, DID, &pq_pub, SignatureScheme::MLDSA44),
            LeafVerdict::Certified,
            "precondition: these inputs certify once the directory is pinned"
        );

        let unpinned = IdentityDirectory {
            by_user: pinned.by_user,
            pinned: false,
        };
        assert!(
            matches!(
                unpinned.leaf_verdict(UID, DID, &pq_pub, SignatureScheme::MLDSA44),
                LeafVerdict::Unverifiable(_)
            ),
            "the same rows must not certify without the local substitutions"
        );
    }

    #[test]
    fn the_certified_key_is_certified() {
        let (dir, ed_pub, pq_pub) = certified_identity();
        assert_eq!(
            dir.leaf_verdict(UID, DID, &pq_pub, SignatureScheme::MLDSA44),
            LeafVerdict::Certified
        );
        assert_eq!(
            dir.leaf_verdict(UID, DID, &ed_pub, SignatureScheme::ED25519),
            LeafVerdict::Certified
        );
    }

    /// A leaf claiming the real `user:device` but carrying the attacker's key must be refused.
    #[test]
    fn a_valid_cert_does_not_vouch_for_a_different_leaf_key() {
        let (dir, _, _) = certified_identity();
        let attacker_key = vec![0xAAu8; 1312];
        assert!(matches!(
            dir.leaf_verdict(UID, DID, &attacker_key, SignatureScheme::MLDSA44),
            LeafVerdict::Uncertified(_)
        ));
    }

    #[test]
    fn a_cert_from_the_wrong_account_key_is_uncertified() {
        let (mut dir, _, pq_pub) = certified_identity();
        let other = AccountSigningKey::from_seed(&[99u8; 32].into());
        dir.set_account_key(UID, Some(other.verifying_key().encode().to_vec()));
        assert!(matches!(
            dir.leaf_verdict(UID, DID, &pq_pub, SignatureScheme::MLDSA44),
            LeafVerdict::Uncertified(_)
        ));
    }

    #[test]
    fn missing_inputs_are_unverifiable_not_certified() {
        let (mut dir, _, pq_pub) = certified_identity();
        assert!(matches!(
            dir.leaf_verdict("nobody", DID, &pq_pub, SignatureScheme::MLDSA44),
            LeafVerdict::Unverifiable(_)
        ));
        assert!(matches!(
            dir.leaf_verdict(UID, "otherdevice", &pq_pub, SignatureScheme::MLDSA44),
            LeafVerdict::Unverifiable(_)
        ));
        dir.set_account_key(UID, None);
        assert!(matches!(
            dir.leaf_verdict(UID, DID, &pq_pub, SignatureScheme::MLDSA44),
            LeafVerdict::Unverifiable(_)
        ));
    }
}
