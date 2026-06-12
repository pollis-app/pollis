//! Signal-style safety numbers / contact verification.
//!
//! The cryptographic root of trust for a user is `users.account_id_pub`
//! (32-byte Ed25519). Every device cert chains to it, so verifying this
//! one value out-of-band transitively covers all of a user's devices.
//!
//! Turso (and anyone who can write to it) is untrusted, so we cannot rely
//! on the server to honestly report a peer's `account_id_pub`. This module
//! pins the first-seen key locally (TOFU) and lets two humans compare a
//! 60-digit safety number derived from both parties' keys.

use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha512};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Iteration count for the fingerprint hash. Matches Signal's
/// NumericFingerprintGenerator default — deliberately slow so a brute
/// force against a truncated fingerprint is infeasible.
const FP_ITERATIONS: usize = 5200;

/// Bump if the fingerprint derivation ever changes so old and new
/// clients never display a matching number under different schemes.
const FP_VERSION: u16 = 0;

#[derive(Debug, Serialize)]
pub struct SafetyNumberInfo {
    /// 60 decimal digits, grouped into 5-digit blocks for display.
    pub safety_number: String,
    /// "unverified" | "verified" | "changed"
    pub status: String,
    /// Peer's current identity version (bumps on account reset).
    pub peer_identity_version: i64,
    /// Both parties' raw `account_id_pub` keys (hex, lowercased) joined
    /// with `:` in canonical order (sorted lexicographically) so the QR
    /// payload is identical on both sides regardless of who opens whose
    /// profile. Decoders compare this directly — no separate "is this
    /// my key or yours" branch needed.
    pub qr_payload: String,
}

/// SHA-512^N over (version || pubkey || stable_id), then 6 blocks of
/// 5 bytes each rendered as 5 decimal digits → a 30-digit per-user
/// fingerprint. Signal's scheme, our own domain/version.
fn fingerprint(pubkey: &[u8], stable_id: &[u8]) -> String {
    let mut hasher = Sha512::new();
    hasher.update(FP_VERSION.to_be_bytes());
    hasher.update(pubkey);
    hasher.update(stable_id);
    let mut hash = hasher.finalize().to_vec();
    for _ in 1..FP_ITERATIONS {
        let mut h = Sha512::new();
        h.update(&hash);
        h.update(pubkey);
        hash = h.finalize().to_vec();
    }

    let mut out = String::with_capacity(30);
    for block in 0..6 {
        let off = block * 5;
        let mut v: u64 = 0;
        for j in 0..5 {
            v = (v << 8) | hash[off + j] as u64;
        }
        out.push_str(&format!("{:05}", v % 100_000));
    }
    out
}

/// Combine both parties' 30-digit fingerprints into one 60-digit number.
/// Sorted so both sides compute the identical string regardless of who
/// opens whose profile, then grouped into 5-digit blocks.
fn combined(my_fp: &str, peer_fp: &str) -> String {
    let (a, b) = if my_fp <= peer_fp {
        (my_fp, peer_fp)
    } else {
        (peer_fp, my_fp)
    };
    let joined = format!("{a}{b}");
    joined
        .as_bytes()
        .chunks(5)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fetch a user's `account_id_pub` + `identity_version` from Turso.
pub(crate) async fn fetch_account_key(
    conn: &libsql::Connection,
    user_id: &str,
) -> Result<(Vec<u8>, i64)> {
    let mut rows = conn
        .query(
            "SELECT account_id_pub, identity_version FROM users WHERE id = ?1",
            libsql::params![user_id],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| Error::Other(anyhow::anyhow!("user {user_id} not found")))?;
    let pubkey: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(0).ok().flatten();
    let pubkey = pubkey
        .ok_or_else(|| Error::Other(anyhow::anyhow!("user {user_id} has no account_id_pub")))?;
    let version: i64 = row.get(1).unwrap_or(0);
    Ok((pubkey, version))
}

/// Compute the safety number for the pair (`my_user_id`, `peer_user_id`)
/// and report its verification status against the local pin.
pub async fn get_safety_number(
    my_user_id: String,
    peer_user_id: String,
    state: &Arc<AppState>,
) -> Result<SafetyNumberInfo> {
    let conn = state.remote_db.conn().await?;
    let (my_pub, _) = fetch_account_key(&conn, &my_user_id).await?;
    let (peer_pub, peer_version) = fetch_account_key(&conn, &peer_user_id).await?;

    let my_fp = fingerprint(&my_pub, my_user_id.as_bytes());
    let peer_fp = fingerprint(&peer_pub, peer_user_id.as_bytes());
    let safety_number = combined(&my_fp, &peer_fp);

    // QR payload: both raw pubkeys (lowercase hex) joined with `:`, in a
    // canonical (sorted) order so both sides scan the same string.
    let my_hex = hex::encode(&my_pub);
    let peer_hex = hex::encode(&peer_pub);
    let (a, b) = if my_hex <= peer_hex {
        (&my_hex, &peer_hex)
    } else {
        (&peer_hex, &my_hex)
    };
    let qr_payload = format!("pollis-key:v{FP_VERSION}:{a}:{b}");

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let pin: Option<(Vec<u8>, i64)> = db
        .conn()
        .query_row(
            "SELECT account_id_pub, verified FROM contact_verification WHERE peer_user_id = ?1",
            rusqlite::params![peer_user_id],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok();

    let status = match pin {
        None => "unverified",
        Some((pinned_pub, _)) if pinned_pub != peer_pub => "changed",
        Some((_, verified)) if verified != 0 => "verified",
        Some(_) => "unverified",
    }
    .to_string();

    Ok(SafetyNumberInfo {
        safety_number,
        status,
        peer_identity_version: peer_version,
        qr_payload,
    })
}

/// Snapshot of every contact for whom the local user has a TOFU pin row,
/// keyed by peer user id. `verified=true` means they were explicitly marked
/// verified by the user; `key_changed=true` means the pin exists but the
/// current `account_id_pub` differs from what was pinned. Drives the
/// shield-icon badges in DM/contact lists and the inline key-changed
/// banner without N round-trips for an N-DM sidebar.
#[derive(Debug, Serialize)]
pub struct PeerVerificationEntry {
    pub peer_user_id: String,
    pub verified: bool,
    pub key_changed: bool,
}

pub async fn list_peer_verifications(
    state: &Arc<AppState>,
) -> Result<Vec<PeerVerificationEntry>> {
    // Read all pinned rows from the local DB first (cheap, single query),
    // then cross-reference against the current `account_id_pub` snapshot in
    // Turso so we can flag mismatches as `key_changed`.
    let pinned: Vec<(String, Vec<u8>, i64)> = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        let mut stmt = db.conn().prepare(
            "SELECT peer_user_id, account_id_pub, verified FROM contact_verification",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
        rows
    };

    if pinned.is_empty() {
        return Ok(Vec::new());
    }

    let conn = state.remote_db.conn().await?;
    let mut out = Vec::with_capacity(pinned.len());
    for (peer_id, pinned_pub, verified) in pinned {
        let current_pub = match fetch_account_key(&conn, &peer_id).await {
            Ok((p, _)) => Some(p),
            // Peer no longer resolves (deleted, network blip) — keep them
            // in the list with the local pin as the only truth. Don't mark
            // them as `key_changed` since we don't actually know.
            Err(_) => None,
        };
        let key_changed = matches!(current_pub, Some(p) if p != pinned_pub);
        out.push(PeerVerificationEntry {
            peer_user_id: peer_id,
            verified: verified != 0,
            key_changed,
        });
    }
    Ok(out)
}

/// Explicitly mark a contact verified (or unverified). Pins the peer's
/// current `account_id_pub`/`identity_version` so a later swap flips the
/// status back to "changed".
pub async fn set_contact_verified(
    peer_user_id: String,
    verified: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;
    let (peer_pub, peer_version) = fetch_account_key(&conn, &peer_user_id).await?;

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    db.conn().execute(
        "INSERT INTO contact_verification \
           (peer_user_id, account_id_pub, identity_version, verified) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(peer_user_id) DO UPDATE SET \
           account_id_pub = excluded.account_id_pub, \
           identity_version = excluded.identity_version, \
           verified = excluded.verified, \
           updated_at = datetime('now')",
        rusqlite::params![peer_user_id, peer_pub, peer_version, verified as i64],
    )?;
    Ok(())
}

/// TOFU pin + change detection for the DM/reconcile path. First sight of a
/// peer's key is pinned silently. A later mismatch updates the pin and
/// clears `verified` (advisory — the caller does not block delivery, the
/// next profile open shows "changed").
pub async fn check_and_pin_account_key(
    state: &Arc<AppState>,
    peer_user_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;
    let (peer_pub, peer_version) = match fetch_account_key(&conn, peer_user_id).await {
        Ok(v) => v,
        // Peer not provisioned yet — nothing to pin, not an error.
        Err(_) => return Ok(()),
    };

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let pinned: Option<Vec<u8>> = db
        .conn()
        .query_row(
            "SELECT account_id_pub FROM contact_verification WHERE peer_user_id = ?1",
            rusqlite::params![peer_user_id],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .ok();

    let mut key_did_change = false;
    match pinned {
        Some(p) if p == peer_pub => {}
        Some(_) => {
            eprintln!(
                "[safety] account_id_pub for {peer_user_id} changed — clearing verified status"
            );
            db.conn().execute(
                "UPDATE contact_verification SET \
                   account_id_pub = ?2, identity_version = ?3, \
                   verified = 0, updated_at = datetime('now') \
                 WHERE peer_user_id = ?1",
                rusqlite::params![peer_user_id, peer_pub, peer_version],
            )?;
            key_did_change = true;
        }
        None => {
            db.conn().execute(
                "INSERT OR IGNORE INTO contact_verification \
                   (peer_user_id, account_id_pub, identity_version, verified) \
                 VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![peer_user_id, peer_pub, peer_version],
            )?;
        }
    }
    // Release the local-DB lock before touching the livekit channel — the
    // sink call is sync but the mutex on `state.livekit` is async, and we
    // never want to hold the local DB lock across an await.
    drop(guard);

    if key_did_change {
        // Surface the change to the open frontend inline (Signal-style
        // "safety number changed" banner). Advisory only — the policy is
        // ADVISORY-with-acknowledge: sends still work, the banner lets the
        // user re-verify out-of-band. Failing to emit is non-fatal; the
        // pin is already updated locally and the next profile open will
        // still report status="changed".
        let sink = state.livekit.lock().await.channel.clone();
        if let Some(ch) = sink {
            let _ = ch.send(crate::realtime::RealtimeEvent::KeyChanged {
                peer_user_id: peer_user_id.to_string(),
                peer_identity_version: peer_version,
            });
        }
    }
    Ok(())
}

/// Batch TOFU pin + change detection for the group-reconcile path.
///
/// Same semantics as [`check_and_pin_account_key`] but bulk-fetches every
/// peer's `account_id_pub` in one Turso query, so a 50-member group costs
/// one round-trip instead of fifty. New peers are pinned silently; an
/// existing pin that no longer matches the server's current key is
/// updated, has its `verified` flag cleared, and emits a `KeyChanged`
/// event so any open conversation surface (DM, group, channel) can show
/// the banner.
///
/// Callers should exclude their own user_id — the local user is not a
/// peer and has no `contact_verification` row.
///
/// Non-fatal: failures are logged and swallowed. The MLS reconcile must
/// continue even if Turso is briefly unreachable; the next reconcile (or
/// the per-message ingest TOFU) will catch up.
pub async fn batch_check_and_pin_account_keys(
    state: &Arc<AppState>,
    peer_user_ids: &[String],
) -> Result<()> {
    if peer_user_ids.is_empty() {
        return Ok(());
    }

    // 1. One Turso SELECT for every peer. Bind by-position to avoid the
    //    quote-and-format SQL-injection foot-gun the surrounding code
    //    uses for its own `IN (...)` lookups (those filter by alphanum
    //    via input scrubbing; we're stricter here because account_id_pub
    //    is the cryptographic root of trust).
    let conn = state.remote_db.conn().await?;
    let placeholders = (1..=peer_user_ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT id, account_id_pub, identity_version FROM users WHERE id IN ({placeholders})"
    );
    let params: Vec<libsql::Value> = peer_user_ids
        .iter()
        .map(|id| libsql::Value::Text(id.clone()))
        .collect();
    let mut server_keys: std::collections::HashMap<String, (Vec<u8>, i64)> =
        std::collections::HashMap::new();
    let mut rows = conn.query(&query, params).await?;
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let pubkey: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(1).ok().flatten();
        let version: i64 = row.get(2).unwrap_or(0);
        if let Some(p) = pubkey {
            server_keys.insert(id, (p, version));
        }
    }

    if server_keys.is_empty() {
        return Ok(());
    }

    // 2. One local-DB SELECT for existing pins covering this batch.
    let mut changed: Vec<(String, i64)> = Vec::new();
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        let mut existing: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        {
            let mut stmt = db.conn().prepare(
                "SELECT peer_user_id, account_id_pub FROM contact_verification",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
            for (id, pubkey) in rows {
                existing.insert(id, pubkey);
            }
        }
        for (peer_id, (server_pub, server_version)) in &server_keys {
            match existing.get(peer_id) {
                Some(p) if p == server_pub => {
                    // No change — leave verified flag alone.
                }
                Some(_) => {
                    db.conn().execute(
                        "UPDATE contact_verification SET \
                           account_id_pub = ?2, identity_version = ?3, \
                           verified = 0, updated_at = datetime('now') \
                         WHERE peer_user_id = ?1",
                        rusqlite::params![peer_id, server_pub, *server_version],
                    )?;
                    eprintln!(
                        "[safety] group-reconcile: account_id_pub for {peer_id} changed — cleared verified"
                    );
                    changed.push((peer_id.clone(), *server_version));
                }
                None => {
                    db.conn().execute(
                        "INSERT OR IGNORE INTO contact_verification \
                           (peer_user_id, account_id_pub, identity_version, verified) \
                         VALUES (?1, ?2, ?3, 0)",
                        rusqlite::params![peer_id, server_pub, *server_version],
                    )?;
                }
            }
        }
    }

    // 3. Emit one KeyChanged event per changed peer. Done outside the
    //    local-DB guard so we never hold the rusqlite lock across an
    //    await on the LiveKit mutex.
    if !changed.is_empty() {
        let sink = state.livekit.lock().await.channel.clone();
        if let Some(ch) = sink {
            for (peer_user_id, peer_identity_version) in changed {
                let _ = ch.send(crate::realtime::RealtimeEvent::KeyChanged {
                    peer_user_id,
                    peer_identity_version,
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_30_digits() {
        let k = [7u8; 32];
        let a = fingerprint(&k, b"user-a");
        let b = fingerprint(&k, b"user-a");
        assert_eq!(a, b);
        assert_eq!(a.len(), 30);
        assert!(a.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn different_keys_differ() {
        assert_ne!(fingerprint(&[1u8; 32], b"x"), fingerprint(&[2u8; 32], b"x"));
    }

    #[test]
    fn combined_is_order_independent() {
        let x = fingerprint(&[1u8; 32], b"alice");
        let y = fingerprint(&[2u8; 32], b"bob");
        assert_eq!(combined(&x, &y), combined(&y, &x));
        assert_eq!(combined(&x, &y).replace(' ', "").len(), 60);
    }
}
