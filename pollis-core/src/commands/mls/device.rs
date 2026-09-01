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

// ── Per-device stable MLS signing key ────────────────────────────────────────

/// Custom scope in `mls_kv` that stores the stable per-device MLS
/// signature public-key bytes. The private side is held by openmls under
/// its own `SignatureKeyPair` scope, looked up by these same bytes.
const DEVICE_SIG_PUB_SCOPE: &str = "PollisDeviceSigPub";

/// The `mls_kv` key holding this device's stable signing pub for one signature
/// scheme.
///
/// Scheme-scoped since #668. Up to v1.7.0 both suites signed Ed25519, so one
/// row per `(user, device)` was the whole story; now [`CS_PQ`] leaves sign
/// ML-DSA-44 while pre-#668 groups still sign Ed25519, and a device holding
/// both is holding two keys at once. #669 retired the classic suite but not
/// this: a device only stops needing its Ed25519 key once every group it is in
/// has actually been migrated, which is a per-group event, not a release.
///
/// Keying on the scheme rather than the suite is deliberate — it is the scheme
/// that determines whether a stored key can verify a given leaf, and two suites
/// sharing a scheme should share a key.
///
/// [`CS_PQ`]: super::provider::CS_PQ
fn device_sig_pub_kv_key(user_id: &str, device_id: &str, scheme: SignatureScheme) -> Vec<u8> {
    format!("{user_id}:{device_id}:{}", scheme as u16).into_bytes()
}

/// The pre-#668 `mls_kv` key: no scheme suffix, always Ed25519.
///
/// Read-only and Ed25519-only. Every device that has ever run a shipped build
/// has its Ed25519 signing pub here and nowhere else, and that key is the one
/// its existing classic groups' leaves are signed with — so dropping the
/// fallback would strand every group on the device, not just cost a rotation.
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
/// **Stable per signature scheme, not per device** (#668). Until v1.7.0 both
/// suites signed Ed25519 and a device had exactly one signing key; now `CS_PQ`
/// leaves sign ML-DSA-44 while any group still on a pre-#668 suite signs
/// Ed25519, and a leaf can only be signed by a key of its suite's scheme. A
/// device in groups of both therefore holds both keys — same `mls_kv`,
/// different rows. Callers pass the scheme they need, which they get from the
/// suite they are operating in (`provider::signature_scheme`) or from the
/// stored group.
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
        eprintln!(
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
/// Like Ed25519, openmls stores an ML-DSA private key as exactly its 32-byte
/// seed (`openmls_rust_crypto`'s `signature_key_gen` calls `to_seed()`), so the
/// same `private()`-to-seed recovery works. Returns `(signing_key, pub_bytes)`;
/// `pub_bytes` is the 1312-byte `mls_signature_pub_pq`.
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
/// A device cert covers BOTH (#668 P4): the classic suite's leaves are Ed25519
/// and the PQ suite's are ML-DSA-44, and a device in groups of both suites holds
/// both keys at once. Certifying only one would leave the other leaf
/// uncertified, which is exactly what cross-signing exists to prevent.
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
    if !crate::commands::account_identity::has_local_account_identity(
        state.as_ref(),
        user_id,
    ).await? {
        return Ok(false);
    }

    // 1. Load or create BOTH stable per-device MLS signing keypairs and capture
    //    their public bytes. Cert v2 (#668) binds both, so a device is never
    //    holding an uncertified leaf key during the classic→PQ overlap. Sync
    //    openmls work inside a scope.
    let (sig_pub_bytes, pq_sig_pub_bytes) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        load_device_cert_pubs(&provider, user_id, device_id)?
    };

    // 2. Read the current identity_version to stamp into the cert.
    //
    //    Via the UNAUTHENTICATED account probe, not the authenticated
    //    `account-status` read, because of the bootstrap pivot documented at
    //    step 4: this runs on a device whose `user_device.mls_signature_pub` is
    //    still NULL, which is the exact column the DS reads to verify a
    //    signature — so a device-signed read here 401s on every first signup and
    //    on every sibling-approval enrollment. A session does not close the gap
    //    either; the enrollment path deliberately publishes on cert-validity
    //    alone because a human approval outlives the session TTL.
    //
    //    `identity_version` is public by construction (it is stamped in the
    //    clear into every device cert every group member reads), so the probe is
    //    the right place for it. Defaults to 1 when the column is NULL —
    //    defensive; it should not happen post-migration-13.
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

    // 4. Write cert + signing pub + issued_at + identity_version into
    //    the remote `user_device` row. Other clients read these columns
    //    before accepting this device into any MLS group.

    // BOOTSTRAP PIVOT — this write sets `mls_signature_pub`, the exact column the
    // DS reads to authenticate a signed request (`auth::verify_request`). Until
    // it's populated the device cannot produce a signature the DS would accept,
    // so the write that *establishes* the credential cannot be authenticated by
    // that credential. Two paths (both use the SAME
    // `/v1/auth/publish-device-cert` endpoint):
    //
    //   * First-device signup — gated by the OTP session (stashed in
    //     `state.bootstrap_session` by `verify_otp`) PLUS cert-validity. The token
    //     is single-use, spent server-side on success.
    //   * Subsequent device (sibling-approval enrollment / Secret-Key recovery) —
    //     gated by **cert-validity ALONE**, no live session. The cert proves
    //     possession of the account key (it verifies against the account's
    //     `account_id_pub`), which is STRONGER than a session and survives a slow
    //     sibling approval that would outlast the session TTL. The request carries
    //     `user_id` in the body (no session to bind it); the DS still requires a
    //     pre-existing `user_device` row and never fails open.
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
            // The token is spent server-side on success. Clear the local
            // copy so the next ensure_device_cert takes the cert-validity
            // path.
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
            // A subsequent device's first cert publish completes its
            // bootstrap; the session it used for register-device /
            // enrollment-request has done its job.
            *state.enrollment_session.lock().await = None;
        }
    }

    eprintln!(
        "[mls] device cert published for {user_id}:{device_id} (identity_version={identity_version})"
    );

    Ok(true)
}

/// One device that needs its cross-signing cert re-signed: its id and both leaf
/// signing keys, decoded from the base64 the DS serves.
///
/// A named type rather than a tuple because the v2 cert (#668) binds BOTH leaf
/// keys and the two are the same shape — an accidental swap at a call site would
/// produce a cert that verifies against the wrong key and fails only for the
/// device it was minted for.
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
/// "Stale" means `cert_identity_version IS NULL` or
/// `cert_identity_version < identity_version` — the cert was signed under a
/// previous account key and no longer chains to the currently-published
/// `account_id_pub`.
///
/// Excluded:
///   - **revoked** rows (`revoked_at IS NOT NULL`, #685) — a revoked device can
///     never rejoin the tree, so re-signing its cert is wasted work and would
///     resurrect a valid-looking cert for a deliberately-retired device;
///   - rows with a NULL leaf pub (`mls_signature_pub` or `mls_signature_pub_pq`)
///     — registered devices that never finished `ensure_device_cert` (or predate
///     the #668 v2 cert), which get their cert when that device next comes online.
///
/// A PURE predicate over rows the Delivery Service supplied (#987).
///
/// The revoked exclusion is enforced twice, on purpose: the caller asks the DS
/// for non-revoked rows, and this filters again on `revoked_at`. A predicate
/// this consequential should not depend on a caller having passed the right
/// flag — re-signing a tombstoned device's cert resurrects a valid-looking
/// credential for a device that was deliberately turned off.
///
/// `pub` so the regression suite drives the real predicate rather than a copy.
/// It is pure now, so it needs no database and no separate integration binary —
/// the libsql/rusqlite `sqlite3_config` clash that forced `tests/
/// resign_stale_certs.rs` into its own process is simply gone.
pub fn stale_cert_candidates(
    rows: &[pollis_api::account_reads::DeviceRow],
    identity_version: i64,
) -> crate::error::Result<Vec<StaleCertDevice>> {
    let mut out = Vec::new();
    for row in rows {
        if row.revoked_at.is_some() {
            continue;
        }
        // A device that never finished `ensure_device_cert` (or predates the
        // #668 v2 cert) has no leaf pub to sign over. It gets its cert when it
        // next comes online, not from here.
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
/// Called in two places:
///   1. `account_identity::reset_identity`, immediately after a
///      rotation — every existing row becomes stale by definition,
///      so this catches them all.
///   2. `pin::unlock`, opportunistically — if a sibling device
///      rotated identity while this device was offline, this
///      device's row is stale on the server and will continue to
///      fail cross-signing verification on every other client until
///      it logs in. Re-signing on unlock means existing fleets
///      self-heal as users come online, without a separate sweep.
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

    // The caller's OWN devices, including the cert columns — served with the
    // management columns because this is the owner asking. Revoked rows are
    // excluded: re-signing a tombstoned device's cert would be re-issuing
    // credentials for a device that was deliberately turned off.
    let rows = crate::commands::ds_reads::devices(state, Some(user_id), Vec::new(), false).await?;
    let devices = stale_cert_candidates(&rows, identity_version as i64)?;

    // Sign every stale device's cert with the account identity key (held only in
    // the OS keystore) BEFORE any remote write, collecting the cert columns. The
    // re-sign never touches `mls_signature_pub` — only the cert columns — so it
    // cannot change a device's DS-auth credential.
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

    // DS seam: re-stamp the cert columns through the Delivery Service (the write
    // API) — user-scoped: the actor re-signs certs for any of THEIR OWN devices,
    // never another user's.
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

    eprintln!(
        "[mls] re-signed {count} device cert(s) for {user_id} at identity_version={identity_version}"
    );

    Ok(count)
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
/// Outcome of verifying every device in a commit's added-devices list.
///
/// The three variants tell the caller (`process_pending_commits_locked`)
/// how to treat the offending commit. NOTE: none of them delete the
/// `mls_commit_log` row — a commit that won the epoch CAS is canonical and
/// immutable, and deleting it forks the group (the ELECTRON-epoch-11
/// incident; see `group_state.rs::VerifyOutcome::Revoked`). Log-DB migration
/// 000005 (#691) now also enforces this at the schema layer: deleting a
/// canonical commit is never safe, so the head-protection and immutability
/// triggers make the old delete physically impossible. See issue #372 for the
/// full rationale.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum VerifyOutcome {
    /// Every added device verified — apply the commit normally.
    Verified,
    /// At least one added device is confirmed REVOKED (tombstoned via
    /// `user_device.revoked_at`), OR the cert chain itself failed
    /// verification (bad signature, wrong identity_version). The commit is
    /// illegitimate, but the caller still APPLIES it to stay on the one
    /// canonical branch (it won the epoch CAS and other members may already
    /// have advanced past it); the revoked device is evicted the
    /// append-only way by a later `reconcile` remove commit. We do NOT delete
    /// the row — that broke the append-only invariant (ELECTRON epoch 11).
    Revoked,
    /// At least one added device is ABSENT — the row doesn't exist
    /// anywhere yet, or required cert columns are NULL. This is the
    /// race / replication-lag case (#372): the device may be
    /// legitimately joining but its `user_device` row hasn't reached
    /// this client's view of Turso yet. The commit must NOT be
    /// deleted; the caller should leave it in place so a later
    /// catch-up can retry verification once the row appears.
    AbsentRetry,
}

/// Decide whether the devices a commit adds are legitimately added, from rows
/// the Delivery Service supplied.
///
/// **The decision stays here.** Since #987 the client holds no database
/// credential, so the ROWS arrive over HTTP — but the loop below is a signature
/// check over a cert chain rooted in the user's own `account_id_pub`, and the DS
/// is explicitly outside the trust boundary (`docs/security-whitepaper.md`). A
/// DS that answered "Verified" would be a DS that could add devices to groups.
/// So it answers with columns and this function answers with a verdict, exactly
/// as when the columns came from a `SELECT`.
///
/// `identity` is `None` when the batch named this user but the DS had no
/// `users` row for them — the same replication-lag case a missing row used to
/// be, and treated identically ([`VerifyOutcome::AbsentRetry`]). Every outcome
/// and every log line below is byte-for-byte what the direct-read version
/// produced for the same inputs.
pub(super) fn verify_added_devices(
    identity: Option<&pollis_api::reads::AddedIdentity>,
    target_user_id: &str,
    device_ids: &[String],
) -> crate::error::Result<VerifyOutcome> {
    if device_ids.is_empty() {
        return Ok(VerifyOutcome::Verified);
    }

    // A missing `users` row or NULL account_id_pub falls into AbsentRetry: the
    // row may simply not have replicated yet.
    let Some(identity) = identity else {
        eprintln!("[mls] verify_added_devices: user {target_user_id} not found — retry");
        return Ok(VerifyOutcome::AbsentRetry);
    };
    let Some(account_id_pub) = identity
        .account_id_pub
        .as_deref()
        .map(|s| super::ds_reads::decode_b64("account_id_pub", s))
        .transpose()?
    else {
        eprintln!("[mls] verify_added_devices: {target_user_id} has no account_id_pub — retry");
        return Ok(VerifyOutcome::AbsentRetry);
    };

    // The map is READ, never drained: `added_device_ids` is a CSV column the DS
    // writes, so a repeated id in it is a shape this function has to survive.
    // Taking rows out would make the second mention of a device look absent,
    // turn a legitimate commit into a permanent `AbsentRetry`, and wedge the
    // replay.
    let mut by_device: std::collections::HashMap<&str, &pollis_api::reads::DeviceCertRow> =
        std::collections::HashMap::with_capacity(identity.devices.len());
    for row in &identity.devices {
        by_device.insert(row.device_id.as_str(), row);
    }

    for did in device_ids {
        let row = match by_device.get(did.as_str()) {
            Some(r) => *r,
            None => {
                // Row absent. Could be (a) revoked + hard-deleted by an
                // older app version (pre-#372 deployment), or (b) just not
                // replicated yet. We can't tell, so default to the safer
                // AbsentRetry — never destroy a commit on ambiguous state.
                eprintln!(
                    "[mls] verify_added_devices: device {did} not registered for {target_user_id} — retry (issue #372)"
                );
                return Ok(VerifyOutcome::AbsentRetry);
            }
        };

        // Tombstone wins — a revoked device is unambiguously not allowed
        // to add itself, regardless of cert column state.
        if row.revoked_at.is_some() {
            let revoked_at = &row.revoked_at;
            eprintln!(
                "[mls] verify_added_devices: device {did} is REVOKED (revoked_at={revoked_at:?})"
            );
            return Ok(VerifyOutcome::Revoked);
        }

        let (cert, issued_at_str, cert_identity_version, mls_sig_pub, mls_sig_pub_pq) = match (
            row.device_cert.as_deref(),
            row.cert_issued_at.as_deref(),
            row.cert_identity_version,
            row.mls_signature_pub.as_deref(),
            row.mls_signature_pub_pq.as_deref(),
        ) {
            (Some(c), Some(t), Some(v), Some(p), Some(q)) => (
                super::ds_reads::decode_b64("device_cert", c)?,
                t,
                v,
                super::ds_reads::decode_b64("mls_signature_pub", p)?,
                super::ds_reads::decode_b64("mls_signature_pub_pq", q)?,
            ),
            _ => {
                // Cert columns NULL on a non-revoked row is the
                // "device row inserted but cert publish hasn't landed
                // yet" race. Same treatment as fully absent.
                eprintln!(
                    "[mls] verify_added_devices: device {did} has no cert columns populated — retry"
                );
                return Ok(VerifyOutcome::AbsentRetry);
            }
        };

        let issued_at: u64 = match issued_at_str.parse() {
            Ok(v) => v,
            Err(e) => {
                // Malformed timestamp is a hard format error, not a race —
                // treat as Revoked so the bad row gets cleaned up.
                eprintln!(
                    "[mls] verify_added_devices: device {did} cert_issued_at unparseable '{issued_at_str}': {e}"
                );
                return Ok(VerifyOutcome::Revoked);
            }
        };

        if let Err(e) = crate::commands::account_identity::verify_device_cert(
            &account_id_pub,
            did,
            &mls_sig_pub,
            &mls_sig_pub_pq,
            cert_identity_version as u32,
            issued_at,
            &cert,
        ) {
            // Cert chain itself failed — unambiguous bad data, not a race.
            eprintln!("[mls] verify_added_devices: device {did} cert verification failed: {e}");
            return Ok(VerifyOutcome::Revoked);
        }
    }

    Ok(VerifyOutcome::Verified)
}
