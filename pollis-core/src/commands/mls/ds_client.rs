//! Signed HTTP client for the Delivery Service's authenticated write endpoints.
//!
//! Pollis has no server-side session/token system: the only credential that maps
//! to a `user_id` server-side is the device's stable MLS signing key
//! (`user_device.mls_signature_pub`). So every write the client routes through
//! the Delivery Service (DS) is signed with that key, and the DS verifies the
//! signature against the registered public half (see
//! `pollis_delivery::auth::verify_request`).
//!
//! Each request carries four headers:
//!
//! | Header                | Value                                            |
//! |-----------------------|--------------------------------------------------|
//! | `X-Pollis-User`       | current `users.id`                               |
//! | `X-Pollis-Device`     | current `user_device.device_id` ULID             |
//! | `X-Pollis-Timestamp`  | unix seconds, decimal ASCII                       |
//! | `X-Pollis-Signature`  | base64 (STANDARD) of the 64-byte Ed25519 sig     |
//!
//! The signature is PureEdDSA over the canonical message
//! `{METHOD}\n{PATH}\n{TIMESTAMP}\n{lowercase hex sha256(body)}` — byte-for-byte
//! what `pollis_delivery::auth::canonical_message` reconstructs and verifies.
//! When the DS has auth disabled (`POLLIS_DS_REQUIRE_AUTH=false`) the headers are
//! simply ignored, so signing every request is harmless in that mode.

use std::sync::Arc;

use openmls_traits::signatures::Signer;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::state::AppState;

use super::device::load_or_create_device_signer;
use super::provider::PollisProvider;

/// Build the canonical signed message, byte-for-byte identical to
/// `pollis_delivery::auth::canonical_message`:
/// `{METHOD}\n{PATH}\n{TIMESTAMP}\n{lowercase hex sha256(body)}` (no trailing
/// newline).
fn canonical_message(method: &str, path: &str, timestamp: i64, body: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push(HEX[(b >> 4) as usize] as char);
        hex.push(HEX[(b & 0x0f) as usize] as char);
    }

    format!("{method}\n{path}\n{timestamp}\n{hex}").into_bytes()
}

/// Current unix time in seconds.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The signing user's id for THIS `AppState`.
///
/// Prefer the instance's own authenticated session (`state.unlock`). Co-located
/// clients (the supported "two clients on one machine" dev workflow) share the
/// global `accounts.json`, so its `last_active_user` is whoever logged in last —
/// not reliably *this* client's user. Signing a DS write under the wrong user
/// makes the device-signature lookup miss its `user_device` row → 401. Fall back
/// to the accounts index only before a session is unlocked (single-user installs
/// / early startup).
async fn current_user_id(state: &Arc<AppState>) -> Result<String> {
    if let Some(u) = state.unlock.lock().await.as_ref() {
        if !u.user_id.is_empty() {
            return Ok(u.user_id.clone());
        }
    }
    let index = crate::accounts::read_accounts_index()?;
    index
        .last_active_user
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no active user for DS request signing")))
}

/// Sign and POST `body` (JSON) to `{pollis_delivery_url}{path}`, attaching the
/// four `X-Pollis-*` auth headers. `path` is the request path only, with leading
/// slash and no query (e.g. `/v1/group-info`) — it must match what the DS sees,
/// since it is bound into the signed canonical message.
///
/// Returns the raw [`reqwest::Response`] so callers map status codes themselves
/// (e.g. 409 → `LostRace` on the commit path).
pub async fn ds_post(
    state: &Arc<AppState>,
    path: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response> {
    let base = state
        .config
        .pollis_delivery_url
        .as_deref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("pollis_delivery_url not configured")))?;

    let user_id = current_user_id(state).await?;
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("device_id not set for DS request signing")))?;

    // The exact bytes we hash MUST be the exact bytes we send — serialize once.
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post serialize: {e}")))?;
    let timestamp = now_unix();
    let message = canonical_message("POST", path, timestamp, &body_bytes);

    // Sign with the device's stable MLS signing key. The provider wraps a !Send
    // rusqlite connection, so all signing is confined to this scope which ends
    // (dropping the guard) before any await below.
    let signature_b64 = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("not signed in for DS request signing")))?;
        let provider = PollisProvider::new(db.conn());
        let (signer, _pub_bytes) = load_or_create_device_signer(&provider, &user_id, &device_id)?;
        let sig = signer
            .sign(&message)
            .map_err(|e| Error::Other(anyhow::anyhow!("ds_post sign: {e:?}")))?;
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(sig)
    };

    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let resp = reqwest::Client::new()
        .post(&url)
        .header("X-Pollis-User", &user_id)
        .header("X-Pollis-Device", &device_id)
        .header("X-Pollis-Timestamp", timestamp.to_string())
        .header("X-Pollis-Signature", signature_b64)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post {path}: {e}")))?;
    Ok(resp)
}
