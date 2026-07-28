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
/// row per `(user, device)` was the whole story; now [`CS_CLASSIC`] leaves sign
/// Ed25519 and [`CS_HYBRID`] leaves sign ML-DSA-44, and a device that is in
/// groups of both suites holds both keys at once. Keying on the scheme rather
/// than the suite is deliberate — it is the scheme that determines whether a
/// stored key can verify a given leaf, and two suites sharing a scheme should
/// share a key.
///
/// [`CS_CLASSIC`]: super::provider::CS_CLASSIC
/// [`CS_HYBRID`]: super::provider::CS_HYBRID
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
/// suites signed Ed25519 and a device had exactly one signing key; now
/// `CS_CLASSIC` leaves sign Ed25519 and `CS_HYBRID` leaves sign ML-DSA-44, and
/// a leaf can only be signed by a key of its suite's scheme. A device in groups
/// of both suites therefore holds both keys — same `mls_kv`, different rows.
/// Callers pass the scheme they need, which they get from the suite they are
/// operating in (`provider::signature_scheme`) or from the stored group.
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

/// Load this device's stable MLS signing key as an `ed25519_dalek::SigningKey`.
///
/// This is the SAME key `ds_client` signs DS requests with and the SAME key the
/// device cert certifies (`user_device.mls_signature_pub`). The closed-overlay
/// relay handshake (`net::overlay`) presents a `pollis_relay::ClientIdentity`
/// built from it, so the handshake signature verifies under the key the offline
/// cert chain binds to the account. Requires the local DB to be open.
///
/// Pinned to Ed25519, not to a suite. This is the DS request-auth credential
/// (`user_device.mls_signature_pub`), which #668 leaves on Ed25519 — see P5.
///
/// Ed25519's openmls `SignatureKeyPair` holds its private half as exactly the
/// 32-byte seed — `SignatureKeyPair::private()` (the `test-utils` accessor)
/// returns it verbatim. Returns `(signing_key, pub_bytes)`; `pub_bytes` is the
/// certified `mls_signature_pub`.
pub fn load_device_signing_key(
    provider: &PollisProvider<'_>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<(ed25519_dalek::SigningKey, Vec<u8>)> {
    let (kp, pub_bytes) =
        load_or_create_device_signer(provider, user_id, device_id, SignatureScheme::ED25519)?;
    let seed: [u8; 32] = kp.private().try_into().map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!(
            "device signing key is not a 32-byte Ed25519 seed"
        ))
    })?;
    Ok((ed25519_dalek::SigningKey::from_bytes(&seed), pub_bytes))
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

    // 2. Read the current identity_version for this user from the remote
    //    `users` table. Defaults to 1 if the column is NULL (shouldn't
    //    happen post-migration-13 but is defensive).
    let conn = state.remote_db.conn().await?;
    let identity_version: u32 = {
        let mut rows = conn
            .query(
                "SELECT identity_version FROM users WHERE id = ?1",
                libsql::params![user_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<i64>(0).unwrap_or(1) as u32,
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "user {user_id} not found while signing device cert"
                )))
            }
        }
    };

    // 3. Sign the cert with the account identity key loaded from the OS
    //    keystore, using the current unix time as `issued_at`.
    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

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
    let body = serde_json::json!({
        "user_id": user_id,
        "device_id": device_id,
        "device_cert": b64.encode(&cert),
        "cert_issued_at": issued_at as i64,
        "cert_identity_version": identity_version,
        "mls_signature_pub": b64.encode(&sig_pub_bytes),
        "mls_signature_pub_pq": b64.encode(&pq_sig_pub_bytes),
    });
    match bootstrap_session {
        Some(token) => {
            // First-device signup: session + cert-validity, single-use.
            crate::commands::mls::ds_post_session_ok(
                state,
                "/v1/auth/publish-device-cert",
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
                "/v1/auth/publish-device-cert",
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

/// Re-sign every stale `user_device` row for `user_id` with the user's
/// current account identity key, stamping each row's `device_cert`,
/// `cert_issued_at`, and `cert_identity_version` to match
/// `users.identity_version`.
///
/// "Stale" means `cert_identity_version IS NULL` or
/// `cert_identity_version < users.identity_version` — i.e. the cert
/// was signed under a previous account key and no longer chains to the
/// currently-published `account_id_pub`.
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
/// Skips rows whose `mls_signature_pub` is NULL — those are devices
/// that were registered but never finished `ensure_device_cert`, and
/// will get their cert when they next come online.
///
/// Returns the number of rows re-signed.
pub async fn resign_stale_device_certs(
    state: &Arc<AppState>,
    user_id: &str,
) -> crate::error::Result<usize> {
    let conn = state.remote_db.conn().await?;

    let identity_version: u32 = {
        let mut rows = conn
            .query(
                "SELECT identity_version FROM users WHERE id = ?1",
                libsql::params![user_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<i64>(0).unwrap_or(1) as u32,
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "user {user_id} not found while re-signing device certs"
                )))
            }
        }
    };

    // Both leaf pubs are needed to rebuild the cert v2 payload. A row whose
    // `mls_signature_pub_pq` is still NULL predates #668 and cannot be re-signed
    // into a v2 cert — it is skipped here and gets its cert when that device next
    // runs `ensure_device_cert`, exactly like the pre-existing NULL-`mls_signature_pub`
    // case above it.
    let devices: Vec<(String, Vec<u8>, Vec<u8>)> = {
        let mut rows = conn
            .query(
                "SELECT device_id, mls_signature_pub, mls_signature_pub_pq FROM user_device \
                 WHERE user_id = ?1 \
                   AND mls_signature_pub IS NOT NULL \
                   AND mls_signature_pub_pq IS NOT NULL \
                   AND (cert_identity_version IS NULL \
                        OR cert_identity_version < ?2)",
                libsql::params![user_id, identity_version as i64],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let did: String = row.get(0)?;
            let pub_bytes: Vec<u8> = row.get(1)?;
            let pq_pub_bytes: Vec<u8> = row.get(2)?;
            out.push((did, pub_bytes, pq_pub_bytes));
        }
        out
    };

    // Sign every stale device's cert with the account identity key (held only in
    // the OS keystore) BEFORE any remote write, collecting the cert columns. The
    // re-sign never touches `mls_signature_pub` — only the cert columns — so it
    // cannot change a device's DS-auth credential.
    let mut signed: Vec<(String, Vec<u8>, String)> = Vec::with_capacity(devices.len());
    for (device_id, sig_pub_bytes, pq_sig_pub_bytes) in devices {
        let issued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

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
        let certs: Vec<serde_json::Value> = signed
            .iter()
            .map(|(device_id, cert, issued_at_str)| {
                serde_json::json!({
                    "device_id": device_id,
                    "device_cert": base64::engine::general_purpose::STANDARD.encode(cert),
                    "cert_issued_at": issued_at_str,
                    "cert_identity_version": identity_version as i64,
                })
            })
            .collect();
        let body = serde_json::json!({ "certs": certs, "user_id": user_id });
        crate::commands::mls::ds_post_ok(state, "/v1/devices/resign", &body).await?;
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
/// whether the rejection-and-delete of the offending `mls_commit_log` row
/// is safe. See issue #372 for the full rationale.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum VerifyOutcome {
    /// Every added device verified — apply the commit normally.
    Verified,
    /// At least one added device is confirmed REVOKED (tombstoned via
    /// `user_device.revoked_at`), OR the cert chain itself failed
    /// verification (bad signature, wrong identity_version). Either way
    /// the commit is illegitimate and the caller may delete it from
    /// `mls_commit_log` so a legit commit can claim the same epoch.
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

pub(super) async fn verify_added_devices(
    conn: &libsql::Connection,
    target_user_id: &str,
    device_ids: &[String],
) -> crate::error::Result<VerifyOutcome> {
    if device_ids.is_empty() {
        return Ok(VerifyOutcome::Verified);
    }

    // Fetch account_id_pub once. A missing `users` row or NULL
    // account_id_pub falls into AbsentRetry: the row may simply not have
    // replicated yet.
    let account_id_pub: Vec<u8> = {
        let mut rows = conn
            .query(
                "SELECT account_id_pub FROM users WHERE id = ?1",
                libsql::params![target_user_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => match row.get::<Option<Vec<u8>>>(0).ok().flatten() {
                Some(b) => b,
                None => {
                    eprintln!(
                        "[mls] verify_added_devices: {target_user_id} has no account_id_pub — retry"
                    );
                    return Ok(VerifyOutcome::AbsentRetry);
                }
            },
            None => {
                eprintln!(
                    "[mls] verify_added_devices: user {target_user_id} not found — retry"
                );
                return Ok(VerifyOutcome::AbsentRetry);
            }
        }
    };

    for did in device_ids {
        let mut rows = conn
            .query(
                "SELECT device_cert, cert_issued_at, cert_identity_version, \
                        mls_signature_pub, revoked_at, mls_signature_pub_pq \
                 FROM user_device WHERE device_id = ?1 AND user_id = ?2",
                libsql::params![did.as_str(), target_user_id],
            )
            .await?;

        let row = match rows.next().await? {
            Some(r) => r,
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

        let cert: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(0).ok().flatten();
        let issued_at_str: Option<String> = row.get::<Option<String>>(1).ok().flatten();
        let cert_identity_version: Option<i64> = row.get::<Option<i64>>(2).ok().flatten();
        let mls_sig_pub: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(3).ok().flatten();
        let revoked_at: Option<String> = row.get::<Option<String>>(4).ok().flatten();
        let mls_sig_pub_pq: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(5).ok().flatten();
        drop(rows);

        // Tombstone wins — a revoked device is unambiguously not allowed
        // to add itself, regardless of cert column state.
        if revoked_at.is_some() {
            eprintln!(
                "[mls] verify_added_devices: device {did} is REVOKED (revoked_at={revoked_at:?})"
            );
            return Ok(VerifyOutcome::Revoked);
        }

        let (cert, issued_at_str, cert_identity_version, mls_sig_pub, mls_sig_pub_pq) =
            match (cert, issued_at_str, cert_identity_version, mls_sig_pub, mls_sig_pub_pq) {
                (Some(c), Some(t), Some(v), Some(p), Some(q)) => (c, t, v, p, q),
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
            eprintln!(
                "[mls] verify_added_devices: device {did} cert verification failed: {e}"
            );
            return Ok(VerifyOutcome::Revoked);
        }
    }

    Ok(VerifyOutcome::Verified)
}
