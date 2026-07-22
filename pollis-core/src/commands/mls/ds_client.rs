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
    // First-party DS write — route through the overlay when it is on (§14.2).
    let resp = crate::net::overlay::http_client(state.overlay.as_ref())
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

/// Claim one of `target_user_id`'s (optionally a specific device's) unclaimed
/// MLS key packages through the DS (`POST /v1/key-packages/claim`) and return its
/// TLS-serialized bytes. Returns `Ok(None)` when the target has no unclaimed
/// package — the DS replies `404`, which is the EXACT counterpart of the direct
/// `UPDATE … RETURNING` path's "no row" outcome, so the add path skips that
/// device identically either way. Any other non-2xx is a hard error.
///
/// Device-signed via [`ds_post`]: the claimer is a fully-enrolled device, and the
/// signature is the only thing the DS binds the claim to (any authenticated user
/// may claim a peer's package — that is how you add them).
pub async fn ds_claim_key_package(
    state: &Arc<AppState>,
    target_user_id: &str,
    target_device_id: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    let body = serde_json::json!({
        "target_user_id": target_user_id,
        "target_device_id": target_device_id,
    });
    let resp = ds_post(state, "/v1/key-packages/claim", &body).await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_claim_key_package {status}: {txt}"
        )));
    }
    #[derive(serde::Deserialize)]
    struct ClaimResp {
        key_package: String,
    }
    let parsed: ClaimResp = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_claim_key_package decode: {e}")))?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&parsed.key_package)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_claim_key_package base64: {e}")))?;
    Ok(Some(bytes))
}

/// Ask the DS to mint a LiveKit **participant** token for `room`. `kind` selects
/// the identity scheme (`"realtime"` / `"voice"` / `"view"`) — the user+device
/// halves are always derived server-side from the verified signer, so a client
/// cannot mint a token as another user/device. Device-signed via [`ds_post`].
/// Returns `(token, ws_url)`. Replaces the on-device `livekit_jwt::make_token`
/// (which held the LiveKit API secret).
pub async fn ds_livekit_token(
    state: &Arc<AppState>,
    room: &str,
    kind: &str,
) -> Result<(String, String)> {
    let body = serde_json::json!({ "room": room, "kind": kind });
    let resp = ds_post(state, "/v1/livekit/token", &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_livekit_token {status}: {txt}")));
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        token: String,
        url: String,
    }
    let parsed: Resp = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_livekit_token decode: {e}")))?;
    Ok((parsed.token, parsed.url))
}

/// Fan out a **content-free** control `payload` to a LiveKit `room` via the DS's
/// server-side `RoomService/SendData` (the admin secret stays server-side).
/// Replaces the on-device `make_admin_token` + Twirp POST. Best-effort — the DS
/// treats a room with no participants (404) as success.
pub async fn ds_livekit_send_data(
    state: &Arc<AppState>,
    room: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let body = serde_json::json!({ "room": room, "payload": payload });
    let resp = ds_post(state, "/v1/livekit/send-data", &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_livekit_send_data {status}: {txt}"
        )));
    }
    Ok(())
}

/// List a voice room's roster via the DS (server-side `ListParticipants`).
/// Returns `(identity, display_name)` pairs; internal participants are already
/// filtered server-side. Replaces `room_service_list_participants`. Desktop-only
/// — mobile has no Rust-side voice roster (see `livekit_stub`).
#[cfg(feature = "media")]
pub async fn ds_livekit_participants(
    state: &Arc<AppState>,
    room: &str,
) -> Result<Vec<(String, String)>> {
    let body = serde_json::json!({ "room": room });
    let resp = ds_post(state, "/v1/livekit/participants", &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_livekit_participants {status}: {txt}"
        )));
    }
    #[derive(serde::Deserialize)]
    struct P {
        identity: String,
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        participants: Vec<P>,
    }
    let parsed: Resp = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_livekit_participants decode: {e}")))?;
    Ok(parsed
        .participants
        .into_iter()
        .map(|p| (p.identity, p.name))
        .collect())
}

/// Mint a short-TTL **read-only** Turso token via the DS. Returns `(token,
/// expires_in_secs)`. Device-signed. Any error (incl. 503 when the DS has no
/// Turso Platform credentials) lets the caller fall back to the baked read-only
/// token, so an unconfigured deploy still reads. See #393.
pub async fn ds_turso_token(state: &Arc<AppState>) -> Result<(String, u64)> {
    let resp = ds_post(state, "/v1/turso/token", &serde_json::json!({})).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_turso_token {status}: {txt}")));
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        token: String,
        expires_in: u64,
    }
    let parsed: Resp = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_turso_token decode: {e}")))?;
    Ok((parsed.token, parsed.expires_in))
}

/// [`ds_post`] for writes that must NOT silently fail: any non-2xx becomes an
/// `Err` carrying the status + body. Use this when the direct-write path it
/// replaces propagated its error (`conn.execute(...).await?`). For best-effort
/// writes (the direct path logged and continued) call [`ds_post`] and log
/// instead.
pub async fn ds_post_ok(state: &Arc<AppState>, path: &str, body: &serde_json::Value) -> Result<()> {
    let resp = ds_post(state, path, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_post {path} {s}: {txt}")));
    }
    Ok(())
}

/// [`ds_post`] when this device can sign (local DB open, device key enrolled);
/// otherwise fall back to the verified-OTP bootstrap session ([`ds_post_session`]
/// with `state.bootstrap_session`). For the account-lifecycle writes reachable
/// from a PRE-ENROLLMENT device — the soft reset offered on the login gate —
/// where no signing key exists yet and the user's authorization is the email
/// OTP they just verified. The DS accepts either credential on these endpoints
/// (`gate_or_session`).
pub async fn ds_post_signed_or_session(
    state: &Arc<AppState>,
    path: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response> {
    let can_sign = state.local_db.lock().await.is_some();
    if can_sign {
        return ds_post(state, path, body).await;
    }
    let token = state.bootstrap_session.lock().await.clone().ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "not signed in and no verified-email session — verify your email again, then retry"
        ))
    })?;
    ds_post_session(state, path, &token, body).await
}

/// [`ds_post_signed_or_session`] for writes that must NOT silently fail: any
/// non-2xx becomes an `Err` carrying the status + body.
pub async fn ds_post_signed_or_session_ok(
    state: &Arc<AppState>,
    path: &str,
    body: &serde_json::Value,
) -> Result<()> {
    let resp = ds_post_signed_or_session(state, path, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_post {path} {s}: {txt}")));
    }
    Ok(())
}

/// Report this device's applied MLS `since` epoch for a conversation to the DS
/// commits endpoint (`GET /v1/commits/{conv}?since=&user_id=&device_id=`), the
/// signal the server-side retention floor is the MIN of across current members
/// (#539, I4 Tier 1). `since` is the client's current local epoch — it still
/// needs every commit `>= since`, so a truthful report can only ever PROTECT its
/// own history from pruning.
///
/// Reads are open on the DS, so this is an unauthenticated GET. Fully best-effort
/// and EVENT-DRIVEN (fires once per catch-up, never polls): any failure — no DS
/// URL, no device context, a network error — is swallowed, leaving the floor
/// conservatively low (Tier 2's hard cap still bounds storage). A short timeout
/// keeps it off the catch-up critical path.
pub async fn ds_report_commit_since(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    since: i64,
) {
    let base = match state.config.pollis_delivery_url.as_deref() {
        Some(b) => b.trim_end_matches('/').to_string(),
        None => return,
    };
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return,
    };
    let url = format!("{base}/v1/commits/{conversation_id}");
    let _ = crate::net::overlay::http_client(state.overlay.as_ref())
        .get(&url)
        .query(&[
            ("since", since.to_string()),
            ("user_id", user_id.to_string()),
            ("device_id", device_id),
        ])
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
}

/// Resolve the DS base URL or error if it isn't configured. Shared by the
/// unauthenticated + session-bearer bootstrap clients below.
fn delivery_base(state: &Arc<AppState>) -> Result<String> {
    state
        .config
        .pollis_delivery_url
        .as_deref()
        .map(|s| s.trim_end_matches('/').to_string())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("pollis_delivery_url not configured")))
}

/// POST `body` (JSON) to `{pollis_delivery_url}{path}` with NO auth headers — the
/// pre-identity OTP endpoints (`request-otp` / `verify-otp`), which the DS gates
/// by the OTP itself, not a device signature or a session. Returns the raw
/// [`reqwest::Response`] so the caller reads the body / maps the status.
pub async fn ds_post_plain(
    state: &Arc<AppState>,
    path: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response> {
    let url = format!("{}{}", delivery_base(state)?, path);
    crate::net::overlay::http_client(state.overlay.as_ref())
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_plain {path}: {e}")))
}

/// Sign-free sibling of [`ds_post`] for the OTP-session-gated bootstrap writes
/// (establish-identity / register-device / publish-device-cert). During bootstrap
/// the device has no MLS signing key yet, so these carry the OTP-session bearer
/// token in `X-Pollis-Session` instead of the four `X-Pollis-*` signature headers.
/// Returns the raw [`reqwest::Response`] so the caller maps the status.
pub async fn ds_post_session(
    state: &Arc<AppState>,
    path: &str,
    session_token: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response> {
    let url = format!("{}{}", delivery_base(state)?, path);
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_session serialize: {e}")))?;
    crate::net::overlay::http_client(state.overlay.as_ref())
        .post(&url)
        .header("X-Pollis-Session", session_token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_session {path}: {e}")))
}

/// [`ds_post_session`] for bootstrap writes that must NOT silently fail: any
/// non-2xx becomes an `Err` carrying the status + body.
pub async fn ds_post_session_ok(
    state: &Arc<AppState>,
    path: &str,
    session_token: &str,
    body: &serde_json::Value,
) -> Result<()> {
    let resp = ds_post_session(state, path, session_token, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_post_session {path} {s}: {txt}"
        )));
    }
    Ok(())
}
