//! Device-certificate-signature authentication for write requests.
//!
//! Pollis has **no server-side session/token system**. The only server-side
//! credential that maps to a `user_id` is the device's MLS signing key:
//! `user_device.mls_signature_pub` — the device's raw 32-byte Ed25519 public
//! key, the same key wrapped in `device_cert` and verified by pollis-core's
//! `verify_device_cert`. So DS auth reuses that cross-signing device identity:
//! the client **signs each write with its Ed25519 device private key**, and the
//! DS verifies the signature against the registered public key. No new token
//! table, no shared secret at rest.
//!
//! ## Signing contract (the client MUST produce exactly this)
//!
//! Every authenticated write carries four headers:
//!
//! | Header                | Value                                            |
//! |-----------------------|--------------------------------------------------|
//! | `X-Pollis-User`       | `user_id` (the `users.id` / `mls` sender)        |
//! | `X-Pollis-Device`     | `device_id` (the `user_device.device_id` ULID)   |
//! | `X-Pollis-Timestamp`  | unix seconds, decimal ASCII                       |
//! | `X-Pollis-Signature`  | base64 (STANDARD) of the 64-byte Ed25519 sig     |
//!
//! The signature is over this **canonical message** (a UTF-8 byte string, `\n`
//! = 0x0A, no trailing newline):
//!
//! ```text
//! {METHOD}\n{PATH}\n{TIMESTAMP}\n{HEX_SHA256_BODY}
//! ```
//!
//! where:
//!   - `METHOD`          — the HTTP method, uppercase ASCII (e.g. `POST`).
//!   - `PATH`            — the request path only, no query (e.g. `/v1/commits`).
//!   - `TIMESTAMP`       — the exact ASCII of `X-Pollis-Timestamp`.
//!   - `HEX_SHA256_BODY` — lowercase hex of `SHA-256(raw_request_body_bytes)`.
//!                         For an empty body this is the hex SHA-256 of zero
//!                         bytes. Binding the body hash stops a captured
//!                         signature from being replayed over a *different*
//!                         commit.
//!
//! The signature is **PureEdDSA (Ed25519)** over that message, produced by the
//! device's MLS signing private key; the verifying key is the raw 32-byte
//! `mls_signature_pub` stored in `user_device` — no length prefix, no TLS
//! wrapper (that is exactly what openmls `SignatureKeyPair::to_public_vec()`
//! returns for the `Ed25519` ciphersuite, and what pollis-core's
//! `verify_device_cert` consumes).
//!
//! ## Replay window
//!
//! A request is rejected if its timestamp is more than [`REPLAY_WINDOW_SECS`]
//! away from the server's clock in either direction. 300s (±5 min) is the
//! standard tradeoff (mirrors AWS SigV4 / Stripe webhooks): wide enough to
//! tolerate device/server clock skew without a time-sync handshake, narrow
//! enough that a captured signature is only briefly replayable. The body-hash
//! binding already prevents cross-request replay; the window bounds *identical*
//! request replay. A true nonce/once-store would close the window entirely but
//! needs shared write state the DS deliberately avoids — out of scope here.
//!
//! ## Never fail open
//!
//! Any error on the auth path — missing/garbled header, DB lookup failure,
//! malformed pubkey, signature decode error — resolves to
//! [`AuthRejection::Unauthorized`]. We never let an error become acceptance.

use axum::http::HeaderMap;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use libsql::Connection;
use sha2::{Digest, Sha256};

use crate::error::AuthRejection;

/// Replay window, in seconds, on either side of the server clock. See module
/// docs for the tradeoff.
pub const REPLAY_WINDOW_SECS: i64 = 300;

const H_USER: &str = "x-pollis-user";
const H_DEVICE: &str = "x-pollis-device";
const H_TIMESTAMP: &str = "x-pollis-timestamp";
const H_SIGNATURE: &str = "x-pollis-signature";

/// The four headers parsed off an authenticated request, plus the
/// authenticated identity once the signature checks out.
struct Credentials {
    user_id: String,
    device_id: String,
    timestamp: i64,
    /// 64-byte Ed25519 signature, already base64-decoded.
    signature: Signature,
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// Build the canonical signed message: `{METHOD}\n{PATH}\n{TS}\n{hex(sha256(body))}`.
/// Public so the pollis-core client and the tests can produce byte-for-byte the
/// same string.
pub fn canonical_message(method: &str, path: &str, timestamp: i64, body: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let body_hash = hasher.finalize();
    let body_hash_hex = hex_lower(&body_hash);
    format!("{method}\n{path}\n{timestamp}\n{body_hash_hex}").into_bytes()
}

/// Lowercase hex with no separators. Avoids pulling in the `hex` crate for one
/// 32-byte digest.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Parse and validate the four auth headers (presence + timestamp window +
/// signature decode). Does NOT touch the DB or verify the signature yet.
fn parse_credentials(headers: &HeaderMap, now: i64) -> Result<Credentials, AuthRejection> {
    let user_id = header_str(headers, H_USER).ok_or(AuthRejection::Unauthorized)?;
    let device_id = header_str(headers, H_DEVICE).ok_or(AuthRejection::Unauthorized)?;
    let timestamp_str = header_str(headers, H_TIMESTAMP).ok_or(AuthRejection::Unauthorized)?;
    let signature_b64 = header_str(headers, H_SIGNATURE).ok_or(AuthRejection::Unauthorized)?;

    if user_id.is_empty() || device_id.is_empty() {
        return Err(AuthRejection::Unauthorized);
    }

    let timestamp: i64 = timestamp_str.parse().map_err(|_| AuthRejection::Unauthorized)?;
    if (now - timestamp).abs() > REPLAY_WINDOW_SECS {
        return Err(AuthRejection::Unauthorized);
    }

    let sig_bytes = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(signature_b64)
            .map_err(|_| AuthRejection::Unauthorized)?
    };
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| AuthRejection::Unauthorized)?;
    let signature = Signature::from_bytes(&sig_arr);

    Ok(Credentials {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        timestamp,
        signature,
    })
}

/// Look up the registered `mls_signature_pub` for `(user_id, device_id)`.
///
/// Returns `Ok(Some(pub))` for a live, enrolled device; `Ok(None)` if the row
/// is absent, revoked, or has a NULL/wrong-length pubkey — all of which the
/// caller treats as "unknown device" → 401. A DB error propagates as `Err` and
/// the caller still rejects (never fails open).
async fn lookup_device_pubkey(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Option<VerifyingKey>> {
    let mut rows = conn
        .query(
            "SELECT mls_signature_pub, revoked_at \
             FROM user_device WHERE device_id = ?1 AND user_id = ?2",
            libsql::params![device_id, user_id],
        )
        .await?;

    let row = match rows.next().await? {
        Some(r) => r,
        None => return Ok(None),
    };

    // A revoked device must not be able to authenticate, regardless of whether
    // its pubkey column is still populated.
    let revoked_at: Option<String> = row.get::<Option<String>>(1).ok().flatten();
    if revoked_at.is_some() {
        return Ok(None);
    }

    let pub_bytes: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(0).ok().flatten();
    let pub_bytes = match pub_bytes {
        Some(b) => b,
        None => return Ok(None),
    };

    let arr: [u8; 32] = match pub_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };
    match VerifyingKey::from_bytes(&arr) {
        Ok(vk) => Ok(Some(vk)),
        Err(_) => Ok(None),
    }
}

/// Full verification of a write request.
///
/// On success returns the authenticated `user_id` so the caller can bind it to
/// `body.sender_id`. The steps, in order, each rejecting on failure:
///   1. all four headers present, timestamp in window, signature decodes;
///   2. the device's `mls_signature_pub` exists, is live (not revoked), 32-byte;
///   3. the Ed25519 signature verifies over the canonical message.
///
/// `now` is unix seconds; injected so tests can pin the clock.
pub async fn verify_request(
    conn: &Connection,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<String, AuthRejection> {
    let creds = parse_credentials(headers, now)?;

    let verifying_key = match lookup_device_pubkey(conn, &creds.user_id, &creds.device_id).await {
        Ok(Some(vk)) => vk,
        // Unknown / revoked device, or a DB error: never fail open.
        Ok(None) | Err(_) => return Err(AuthRejection::Unauthorized),
    };

    let message = canonical_message(method, path, creds.timestamp, body);
    verifying_key
        .verify(&message, &creds.signature)
        .map_err(|_| AuthRejection::Unauthorized)?;

    Ok(creds.user_id)
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
