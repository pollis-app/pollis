//! Per-device stable MLS signing keys and cross-signing certificates.
//!
//! Each device has a single stable MLS signing keypair (so every KeyPackage
//! it ever ships is covered by one `device_cert` in `user_device`). The
//! cert chains the device's signing pub to the user's `account_id_pub`,
//! and is re-signed whenever the account identity rotates.

use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;

use crate::state::AppState;

use super::provider::{PollisProvider, CS};

// ── Per-device stable MLS signing key ────────────────────────────────────────

/// Custom scope in `mls_kv` that stores the stable per-device MLS
/// signature public-key bytes. The private side is held by openmls under
/// its own `SignatureKeyPair` scope, looked up by these same bytes.
const DEVICE_SIG_PUB_SCOPE: &str = "PollisDeviceSigPub";

fn load_stable_device_sig_pub_bytes(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<Option<Vec<u8>>> {
    let key = format!("{user_id}:{device_id}").into_bytes();
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

fn store_stable_device_sig_pub_bytes(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
    pub_bytes: &[u8],
) -> crate::error::Result<()> {
    let key = format!("{user_id}:{device_id}").into_bytes();
    conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![DEVICE_SIG_PUB_SCOPE, key, pub_bytes],
    )?;
    Ok(())
}

/// Return the stable MLS signing keypair for this device, creating it if
/// missing. All key packages and group creation on this device MUST use
/// this keypair so the device-level cross-signing cert in `user_device`
/// covers every leaf node this device produces.
///
/// Returns `(SignatureKeyPair, pub_bytes)`. The pub_bytes are also what
/// gets signed into the `device_cert` in `user_device`.
pub fn load_or_create_device_signer(
    provider: &PollisProvider<'_>,
    user_id: &str,
    device_id: &str,
) -> crate::error::Result<(SignatureKeyPair, Vec<u8>)> {
    // Fast path: pub bytes are stashed → recover the private side from
    // openmls storage and return.
    if let Some(pub_bytes) = load_stable_device_sig_pub_bytes(
        provider.raw_conn(),
        user_id,
        device_id,
    )? {
        if let Some(kp) = SignatureKeyPair::read(
            provider.storage(),
            &pub_bytes,
            CS.signature_algorithm(),
        ) {
            return Ok((kp, pub_bytes));
        }
        // Pub bytes stashed but the private side is gone (e.g. mls_kv
        // got partially wiped). Fall through to regenerate.
        eprintln!(
            "[mls] stable device signer pub present but private missing for {user_id}:{device_id} — regenerating"
        );
    }

    // Slow path: create, store, stash.
    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key gen: {e}")))?;
    sig_keys
        .store(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig key store: {e}")))?;
    let pub_bytes = sig_keys.to_public_vec();
    store_stable_device_sig_pub_bytes(provider.raw_conn(), user_id, device_id, &pub_bytes)?;
    Ok((sig_keys, pub_bytes))
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

    // 1. Load or create the stable per-device MLS signing keypair and
    //    capture its public bytes. Sync openmls work inside a scope.
    let sig_pub_bytes = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        let (_sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, user_id, device_id)?;
        sig_pub_bytes
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
        identity_version,
        issued_at,
    )
    .await?;

    // 4. Write cert + signing pub + issued_at + identity_version into
    //    the remote `user_device` row. Other clients read these columns
    //    before accepting this device into any MLS group.
    //
    // `cert_issued_at` is stored as a decimal string of unix seconds —
    // the migration created the column as TEXT, and we need lossless
    // round-trip to u64 for signature verification later.
    let issued_at_str = issued_at.to_string();

    conn.execute(
        "UPDATE user_device \
         SET device_cert = ?1, \
             cert_issued_at = ?2, \
             cert_identity_version = ?3, \
             mls_signature_pub = ?4 \
         WHERE device_id = ?5",
        libsql::params![
            cert,
            issued_at_str,
            identity_version as i64,
            sig_pub_bytes,
            device_id
        ],
    )
    .await?;

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

    let devices: Vec<(String, Vec<u8>)> = {
        let mut rows = conn
            .query(
                "SELECT device_id, mls_signature_pub FROM user_device \
                 WHERE user_id = ?1 \
                   AND mls_signature_pub IS NOT NULL \
                   AND (cert_identity_version IS NULL \
                        OR cert_identity_version < ?2)",
                libsql::params![user_id, identity_version as i64],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let did: String = row.get(0)?;
            let pub_bytes: Vec<u8> = row.get(1)?;
            out.push((did, pub_bytes));
        }
        out
    };

    let mut count = 0;
    for (device_id, sig_pub_bytes) in devices {
        let issued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let cert = crate::commands::account_identity::sign_device_cert(
            state.as_ref(),
            user_id,
            &device_id,
            &sig_pub_bytes,
            identity_version,
            issued_at,
        )
        .await?;

        let issued_at_str = issued_at.to_string();
        conn.execute(
            "UPDATE user_device \
             SET device_cert = ?1, \
                 cert_issued_at = ?2, \
                 cert_identity_version = ?3 \
             WHERE device_id = ?4 AND user_id = ?5",
            libsql::params![
                cert,
                issued_at_str,
                identity_version as i64,
                device_id.clone(),
                user_id
            ],
        )
        .await?;
        count += 1;
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
pub(super) async fn verify_added_devices(
    conn: &libsql::Connection,
    target_user_id: &str,
    device_ids: &[String],
) -> crate::error::Result<bool> {
    if device_ids.is_empty() {
        return Ok(true);
    }

    // Fetch account_id_pub once.
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
                        "[mls] verify_added_devices: {target_user_id} has no account_id_pub"
                    );
                    return Ok(false);
                }
            },
            None => {
                eprintln!(
                    "[mls] verify_added_devices: user {target_user_id} not found"
                );
                return Ok(false);
            }
        }
    };

    for did in device_ids {
        let mut rows = conn
            .query(
                "SELECT device_cert, cert_issued_at, cert_identity_version, mls_signature_pub \
                 FROM user_device WHERE device_id = ?1 AND user_id = ?2",
                libsql::params![did.as_str(), target_user_id],
            )
            .await?;

        let row = match rows.next().await? {
            Some(r) => r,
            None => {
                eprintln!(
                    "[mls] verify_added_devices: device {did} not registered for {target_user_id}"
                );
                return Ok(false);
            }
        };

        let cert: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(0).ok().flatten();
        let issued_at_str: Option<String> = row.get::<Option<String>>(1).ok().flatten();
        let cert_identity_version: Option<i64> = row.get::<Option<i64>>(2).ok().flatten();
        let mls_sig_pub: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(3).ok().flatten();
        drop(rows);

        let (cert, issued_at_str, cert_identity_version, mls_sig_pub) =
            match (cert, issued_at_str, cert_identity_version, mls_sig_pub) {
                (Some(c), Some(t), Some(v), Some(p)) => (c, t, v, p),
                _ => {
                    eprintln!(
                        "[mls] verify_added_devices: device {did} has no cert columns populated"
                    );
                    return Ok(false);
                }
            };

        let issued_at: u64 = match issued_at_str.parse() {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[mls] verify_added_devices: device {did} cert_issued_at unparseable '{issued_at_str}': {e}"
                );
                return Ok(false);
            }
        };

        if let Err(e) = crate::commands::account_identity::verify_device_cert(
            &account_id_pub,
            did,
            &mls_sig_pub,
            cert_identity_version as u32,
            issued_at,
            &cert,
        ) {
            eprintln!(
                "[mls] verify_added_devices: device {did} cert verification failed: {e}"
            );
            return Ok(false);
        }
    }

    Ok(true)
}
