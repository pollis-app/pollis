//! Signal-style safety numbers and contact verification.
//!
//! The root of trust for a user is `users.account_id_pub` (Ed25519); every device
//! cert chains to it. The server is untrusted, so the first-seen key is pinned
//! locally (TOFU) and two people can compare a 60-digit number derived from both
//! keys.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha512};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Fingerprint hash iterations: Signal's NumericFingerprintGenerator default,
/// slow enough that brute-forcing a truncated fingerprint is infeasible.
const FP_ITERATIONS: usize = 5200;

/// Bump if the derivation changes, so old and new clients never show a matching
/// number under different schemes.
const FP_VERSION: u16 = 0;

#[derive(Debug, Serialize)]
pub struct SafetyNumberInfo {
    /// 60 digits in 5-digit blocks.
    pub safety_number: String,
    pub status: String,
    /// Bumps on account reset.
    pub peer_identity_version: i64,
    /// Both raw keys (lowercase hex) joined with `:` in sorted order, so the QR
    /// payload is identical on both sides.
    pub qr_payload: String,
}

/// SHA-512^N over (version || pubkey || stable_id), rendered as six 5-digit
/// blocks: Signal's scheme with our own version.
fn fingerprint(pubkey: &[u8], stable_id: &[u8]) -> String {
    let mut hasher = Sha512::new();
    hasher.update(FP_VERSION.to_be_bytes());
    hasher.update(pubkey);
    hasher.update(stable_id);
    let mut hash = [0u8; 64];
    hash.copy_from_slice(&hasher.finalize());
    for _ in 1..FP_ITERATIONS {
        let mut h = Sha512::new();
        h.update(hash);
        h.update(pubkey);
        hash.copy_from_slice(&h.finalize());
    }

    let mut out = String::with_capacity(30);
    for block in 0..6 {
        let off = block * 5;
        let mut v: u64 = 0;
        for j in 0..5 {
            v = (v << 8) | hash[off + j] as u64;
        }
        let _ = write!(out, "{:05}", v % 100_000);
    }
    out
}

/// Combine both 30-digit fingerprints, sorted so both sides compute the same
/// string.
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


/// Safety number for the pair, with its status against the local pin.
pub async fn get_safety_number(
    my_user_id: String,
    peer_user_id: String,
    state: &Arc<AppState>,
) -> Result<SafetyNumberInfo> {
    let keys = crate::commands::ds_reads::account_keys(
        state,
        &[my_user_id.clone(), peer_user_id.clone()],
    )
    .await?;
    let find = |id: &str| {
        keys.iter()
            .find(|(u, _, _)| u == id)
            .map(|(_, k, v)| (k.clone(), *v))
    };
    let (my_pub, _) = find(&my_user_id).ok_or_else(|| {
        Error::Other(anyhow::anyhow!("no account_id_pub for user {my_user_id}"))
    })?;
    let (peer_pub, peer_version) = find(&peer_user_id).ok_or_else(|| {
        Error::Other(anyhow::anyhow!("no account_id_pub for user {peer_user_id}"))
    })?;

    let my_fp = fingerprint(&my_pub, my_user_id.as_bytes());
    let peer_fp = fingerprint(&peer_pub, peer_user_id.as_bytes());
    let safety_number = combined(&my_fp, &peer_fp);

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

/// A contact with a local TOFU pin. `key_changed` means the current key differs
/// from the pinned one. Drives the sidebar shield badges and key-changed banner.
#[derive(Debug, Serialize)]
pub struct PeerVerificationEntry {
    pub peer_user_id: String,
    pub verified: bool,
    pub key_changed: bool,
}

pub async fn list_peer_verifications(
    state: &Arc<AppState>,
) -> Result<Vec<PeerVerificationEntry>> {
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

    // One request for every pinned peer; the DS chunks its own binds.
    let ids: Vec<String> = pinned.iter().map(|(id, _, _)| id.clone()).collect();
    let server_keys: std::collections::HashMap<String, Vec<u8>> =
        crate::commands::ds_reads::account_keys(state, &ids)
            .await?
            .into_iter()
            .map(|(id, key, _)| (id, key))
            .collect();

    let mut out = Vec::with_capacity(pinned.len());
    for (peer_id, pinned_pub, verified) in pinned {
        // Absent from the result means the peer no longer resolves: keep the local pin
        // and don't claim `key_changed`, since we don't know.
        let key_changed = matches!(server_keys.get(&peer_id), Some(p) if *p != pinned_pub);
        out.push(PeerVerificationEntry {
            peer_user_id: peer_id,
            verified: verified != 0,
            key_changed,
        });
    }
    Ok(out)
}

/// Mark a contact verified or unverified, pinning their current key so a later
/// swap shows "changed".
pub async fn set_contact_verified(
    peer_user_id: String,
    verified: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let (peer_pub, peer_version) =
        crate::commands::ds_reads::account_key(state, &peer_user_id).await?;

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

/// TOFU pin and change detection. First sight pins silently; a mismatch updates
/// the pin and clears `verified`. Advisory: delivery is never blocked.
pub async fn check_and_pin_account_key(
    state: &Arc<AppState>,
    peer_user_id: &str,
) -> Result<()> {
    let (peer_pub, peer_version) =
        match crate::commands::ds_reads::account_key(state, peer_user_id).await {
            Ok(v) => v,
            // Peer not provisioned yet: nothing to pin.
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
            crate::diag!(
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
    // Never hold the local DB lock across the await on `state.livekit`.
    drop(guard);

    if key_did_change {
        // Advisory banner only; sends still work. If emitting fails, the next profile
        // open still reports "changed".
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

/// Batch form of [`check_and_pin_account_key`] for group reconcile: one request
/// for every peer, and a `KeyChanged` event per changed pin.
///
/// Exclude the caller's own id. Failures are swallowed so the MLS reconcile
/// continues; the next reconcile or ingest catches up.
pub async fn batch_check_and_pin_account_keys(
    state: &Arc<AppState>,
    peer_user_ids: &[String],
) -> Result<()> {
    if peer_user_ids.is_empty() {
        return Ok(());
    }

    let server_keys: std::collections::HashMap<String, (Vec<u8>, i64)> =
        crate::commands::ds_reads::account_keys(state, peer_user_ids)
            .await?
            .into_iter()
            .map(|(id, key, version)| (id, (key, version)))
            .collect();

    if server_keys.is_empty() {
        return Ok(());
    }

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
                }
                Some(_) => {
                    db.conn().execute(
                        "UPDATE contact_verification SET \
                           account_id_pub = ?2, identity_version = ?3, \
                           verified = 0, updated_at = datetime('now') \
                         WHERE peer_user_id = ?1",
                        rusqlite::params![peer_id, server_pub, *server_version],
                    )?;
                    crate::diag!(
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

    // Outside the local-DB guard: never hold it across the LiveKit mutex await.
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

    /// Exact digits a (key, id) pair must produce. Shape tests would pass a change to
    /// the hash chain that silently renumbers every safety number, which users would
    /// read as a MITM. Never change without bumping `FP_VERSION`.
    #[test]
    fn fingerprint_matches_its_pinned_vectors() {
        assert_eq!(
            fingerprint(&[7u8; 32], b"user-a"),
            "594809296990642088583120794182"
        );
        assert_eq!(
            fingerprint(&[3u8; 32], b"pollis"),
            "182206983802156935973920110564"
        );
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
