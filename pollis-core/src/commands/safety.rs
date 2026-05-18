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
async fn fetch_account_key(
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
    })
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
