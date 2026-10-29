//! Signed HTTP client for the Delivery Service's authenticated write endpoints.
//!
//! The only credential that maps to a `user_id` server-side is the device's
//! stable PQ MLS signing key (`user_device.mls_signature_pub_pq`), so every
//! signed request carries four headers:
//!
//! | Header                | Value                                            |
//! |-----------------------|--------------------------------------------------|
//! | `X-Pollis-User`       | current `users.id`                               |
//! | `X-Pollis-Device`     | current `user_device.device_id` ULID             |
//! | `X-Pollis-Timestamp`  | unix seconds, decimal ASCII                       |
//! | `X-Pollis-Signature`  | base64 (STANDARD) of the 2420-byte ML-DSA-44 sig |
//!
//! The signature covers `canonical_message`, verified by
//! `pollis_delivery::auth::verify_request`.

use std::sync::Arc;

use openmls::prelude::Ciphersuite;
use openmls_traits::signatures::Signer;
use sha2::{Digest, Sha256};

use pollis_api::{ClientRequest, DsRequest};

use crate::error::{Error, Result};
use crate::state::AppState;

use super::device::load_or_create_device_signer;
use super::provider::PollisProvider;

/// The canonical signed message, byte-for-byte identical to
/// `pollis_delivery::auth::canonical_message`:
/// `{METHOD}\n{PATH}\n{TIMESTAMP}\n{lowercase hex sha256(body)}` (no trailing
/// newline).
fn canonical_message(method: &str, path: &str, timestamp: i64, body: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let hex = hex::encode(hasher.finalize());

    format!("{method}\n{path}\n{timestamp}\n{hex}").into_bytes()
}


/// The signing user's id for THIS `AppState`.
///
/// Prefers the instance's own session: co-located dev clients share
/// `accounts.json`, whose `last_active_user` may be the other client's user, and
/// signing as the wrong user is a 401. The index is the fallback before unlock.
pub(crate) async fn current_user_id(state: &Arc<AppState>) -> Result<String> {
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

/// Sign and POST a typed request body to [`DsRequest::PATH`], attaching the four
/// `X-Pollis-*` auth headers. `B` is the same `pollis-api` type the DS parses, so
/// a field mismatch fails to compile (#918).
///
/// Returns the raw [`reqwest::Response`] so callers map status codes themselves.
pub async fn ds_post<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    // Sign and send in a NON-generic core so each endpoint does not monomorphize
    // the whole signing path (mobile binary size).
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post serialize: {e}")))?;
    ds_post_bytes(state, B::PATH, B::IDEMPOTENT, body_bytes).await
}

async fn ds_post_bytes(
    state: &Arc<AppState>,
    path: &str,
    idempotent: bool,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response> {
    // Resolved first so a missing URL reports as "not configured".
    let base = delivery_base(state)?;

    let user_id = current_user_id(state).await?;
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("device_id not set for DS request signing")))?;

    // A device the DS already called revoked cannot sign anything acceptable
    // (#1338). Bound to this exact device, so it never blocks another account.
    if state.is_revoked_device(&user_id, &device_id) {
        return Err(Error::DeviceRevoked);
    }

    // The exact bytes hashed MUST be the exact bytes sent.
    let timestamp = crate::util::now_unix() as i64;
    let message = canonical_message("POST", path, timestamp, &body_bytes);

    // The provider wraps a !Send rusqlite connection, so signing is confined to
    // this scope, which ends before any await below.
    let signature_b64 = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("not signed in for DS request signing")))?;
        // Request auth uses the ML-DSA-44 key (#668), so a future break of
        // Ed25519 cannot forge writes from captured traffic.
        let provider = PollisProvider::new(db.conn());
        let (signer, _pub_bytes) = load_or_create_device_signer(
            &provider,
            &user_id,
            &device_id,
            openmls::prelude::SignatureScheme::MLDSA44,
        )?;
        let sig = signer
            .sign(&message)
            .map_err(|e| Error::Other(anyhow::anyhow!("ds_post sign: {e:?}")))?;
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(sig)
    };

    let url = format!("{base}{path}");
    let overlay = state.overlay_handle();
    let request = with_dev_key(state, crate::net::overlay::http_client(overlay.as_deref()).post(&url))
        .header("X-Pollis-User", &user_id)
        .header("X-Pollis-Device", &device_id)
        .header("X-Pollis-Timestamp", timestamp.to_string())
        .header("X-Pollis-Signature", signature_b64)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    let resp = send_ds_resending(path, request, idempotent)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post {path}: {}", error_chain(&e))))?;
    if is_device_revoked_response(&resp) {
        if note_revoked_response(state, &user_id, &device_id).await {
            crate::diag!("[ds] {path}: this device was revoked from the account; signing out");
        }
        return Err(Error::DeviceRevoked);
    }
    Ok(resp)
}

/// Record a `device_revoked` answer to a request signed by `(user_id,
/// device_id)` (#1338), but only while that device is still this process's
/// device, so a late response can never wipe the session that replaced it.
/// Returns whether it newly recorded one.
#[doc(hidden)]
pub async fn note_revoked_response(state: &Arc<AppState>, user_id: &str, device_id: &str) -> bool {
    let still_this_device = state.device_id.lock().await.as_deref() == Some(device_id);
    let signed_in_as = crate::accounts::read_accounts_index()
        .unwrap_or_default()
        .last_active_user;
    if !still_this_device || signed_in_as.as_deref() != Some(user_id) {
        return false;
    }
    if state.is_revoked_device(user_id, device_id) {
        return false;
    }
    state.mark_revoked_device(user_id, device_id);
    true
}

/// The DS's answer to a request signed by a revoked device (#1338): 401 plus
/// `x-pollis-auth-error: device_revoked`.
fn is_device_revoked_response(resp: &reqwest::Response) -> bool {
    resp.status() == reqwest::StatusCode::UNAUTHORIZED
        && resp
            .headers()
            .get("x-pollis-auth-error")
            .and_then(|v| v.to_str().ok())
            == Some("device_revoked")
}

/// Claim one of `target_user_id`'s (optionally a specific device's) unclaimed
/// MLS key packages and return its TLS bytes. `Ok(None)` (DS 404) means the
/// target has no unclaimed package in `ciphersuite`; any other non-2xx is an
/// error.
///
/// `ciphersuite` is not always `CS_PQ`: adding a device to a group that has not
/// migrated yet must claim in that group's suite.
pub async fn ds_claim_key_package(
    state: &Arc<AppState>,
    target_user_id: &str,
    target_device_id: Option<&str>,
    ciphersuite: Ciphersuite,
) -> Result<Option<Vec<u8>>> {
    let body = pollis_api::devices::ClaimKeyPackageBody {
        target_user_id: target_user_id.to_string(),
        target_device_id: target_device_id.map(str::to_string),
        ciphersuite: Some(i64::from(u16::from(ciphersuite))),
    };
    let resp = ds_post(state, &body).await?;
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
    let parsed = decode_response::<pollis_api::devices::ClaimKeyPackageBody>(resp).await?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&parsed.key_package)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_claim_key_package base64: {e}")))?;
    Ok(Some(bytes))
}

/// Ask the DS to mint a LiveKit participant token for `room`, returning
/// `(token, ws_url)`. `kind` selects the identity scheme (`"realtime"` /
/// `"voice"` / `"view"`); user and device always come from the verified signer.
pub async fn ds_livekit_token(
    state: &Arc<AppState>,
    room: &str,
    kind: &str,
) -> Result<(String, String)> {
    let device_id = state.device_id.lock().await.clone();
    // `user_id`/`device_id` are the DS's no-auth fallback (`broker::resolve_user`);
    // with auth on they are ignored.
    let body = pollis_api::broker::LivekitTokenBody {
        room: room.to_string(),
        kind: Some(kind.to_string()),
        user_id: Some(current_user_id(state).await?),
        device_id,
    };
    let parsed = ds_post_json(state, &body).await?;

    // The DS's URL is authoritative (it is the server that accepts this JWT);
    // resolved here so no caller can dial the compiled-in config instead.
    let url = resolve_livekit_url(&parsed.url, &state.config.livekit_url);
    Ok((parsed.token, url))
}

/// Precedence for which LiveKit address to dial: the DS's answer wins, the
/// compiled-in config is only a fallback.
pub fn resolve_livekit_url(ds_url: &str, config_url: &str) -> String {
    if ds_url.trim().is_empty() {
        config_url.to_string()
    } else {
        ds_url.to_string()
    }
}

#[cfg(test)]
mod livekit_url_tests {
    use super::resolve_livekit_url;

    /// A DS-supplied address must never be discarded for the compiled-in one.
    #[test]
    fn ds_url_always_wins_over_config() {
        assert_eq!(
            resolve_livekit_url("wss://eu.pollis.com", "wss://rtc.pollis.com"),
            "wss://eu.pollis.com"
        );
    }

    /// Self-host case: a DS with no LIVEKIT_URL set degrades to the compiled-in
    /// default, never to an empty dial.
    #[test]
    fn empty_ds_url_falls_back_to_config() {
        assert_eq!(
            resolve_livekit_url("", "wss://rtc.pollis.com"),
            "wss://rtc.pollis.com"
        );
        assert_eq!(
            resolve_livekit_url("   ", "wss://rtc.pollis.com"),
            "wss://rtc.pollis.com"
        );
    }

    #[test]
    fn both_empty_stays_empty() {
        assert_eq!(resolve_livekit_url("", ""), "");
    }
}

/// Fan out a **content-free** control `payload` to a LiveKit `room` via the DS's
/// server-side `RoomService/SendData`. The DS treats an empty room as success.
pub async fn ds_livekit_send_data(
    state: &Arc<AppState>,
    room: &str,
    payload: serde_json::Value,
) -> Result<()> {
    // No-auth fallback for the acting user — see `ds_livekit_token`.
    let body = pollis_api::broker::LivekitSendDataBody {
        room: room.to_string(),
        payload,
        user_id: Some(current_user_id(state).await?),
    };
    let resp = ds_post(state, &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_livekit_send_data {status}: {txt}"
        )));
    }
    Ok(())
}

/// List a voice room's roster via the DS, each participant already resolved to a
/// user (#836): only the DS holds the per-room identity key. Desktop-only.
#[cfg(feature = "media")]
pub async fn ds_livekit_participants(
    state: &Arc<AppState>,
    room: &str,
) -> Result<Vec<ResolvedIdentity>> {
    // No-auth fallback for the acting user — see `ds_livekit_token`.
    let body = pollis_api::broker::LivekitParticipantsBody {
        room: room.to_string(),
        user_id: Some(current_user_id(state).await?),
    };
    let resp = ds_post(state, &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_livekit_participants {status}: {txt}"
        )));
    }
    let parsed =
        decode_response::<pollis_api::broker::LivekitParticipantsBody>(resp).await?;
    Ok(parsed.participants)
}

/// One participant identity resolved back to a Pollis user (#836, #922).
#[cfg(feature = "media")]
pub use pollis_api::broker::ResolvedIdentity;

/// Ask the DS to resolve opaque LiveKit participant identities back to users
/// (#836); only the DS holds the per-room key.
///
/// `room` is the **logical** room (conversation id / `inbox-<user>` /
/// `call-<ulid>`), not the LiveKit pseudonym. Unresolvable identities are absent
/// from the result.
#[cfg(feature = "media")]
pub async fn ds_livekit_identities(
    state: &Arc<AppState>,
    room: &str,
    identities: Vec<String>,
) -> Result<Vec<ResolvedIdentity>> {
    if identities.is_empty() {
        return Ok(Vec::new());
    }
    // No-auth fallback for the acting user — see `ds_livekit_token`.
    let body = pollis_api::broker::LivekitIdentitiesBody {
        room: room.to_string(),
        identities,
        user_id: Some(current_user_id(state).await?),
    };
    let parsed = ds_post_json(state, &body).await?;
    Ok(parsed.identities)
}

/// [`ds_post`] plus decoding the ONE response body this endpoint declares (#922),
/// so reading a field the server does not send is a compile error.
///
/// Any non-2xx is an `Err`; a caller that branches on a specific status uses
/// [`ds_post`] and reads it itself.
pub async fn ds_post_json<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<B::Response> {
    let resp = reject_non_2xx(ds_post(state, body).await?, B::PATH, "ds_post").await?;
    decode_response::<B>(resp).await
}

/// Pass a 2xx response through; turn anything else into an `Err` carrying the
/// status and the body text, labelled `label`.
async fn reject_non_2xx(
    resp: reqwest::Response,
    path: &str,
    label: &str,
) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let txt = resp.text().await.unwrap_or_default();
    Err(DsRefused::error(label, path, status, txt))
}

/// A non-2xx answer from the DS, kept typed inside `Error::Other` so a caller
/// can `downcast_ref::<DsRefused>()` the status (the durable outbox, #1335).
/// `Display` is `"{label} {path} {status}: {body}"`.
#[derive(Debug)]
pub struct DsRefused {
    pub label: String,
    pub path: String,
    pub status: reqwest::StatusCode,
    pub body: String,
}

impl DsRefused {
    fn error(label: &str, path: &str, status: reqwest::StatusCode, body: String) -> Error {
        Error::Other(anyhow::Error::new(DsRefused {
            label: label.to_string(),
            path: path.to_string(),
            status,
            body,
        }))
    }
}

impl std::fmt::Display for DsRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}: {}", self.label, self.path, self.status, self.body)
    }
}

impl std::error::Error for DsRefused {}

/// Decode a 2xx body into the endpoint's declared response type, for callers
/// that inspect the status first.
pub(crate) async fn decode_response<B: DsRequest>(
    resp: reqwest::Response,
) -> Result<B::Response> {
    let path = B::PATH;
    resp.json::<B::Response>()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post {path} decode: {e}")))
}

/// [`ds_post`] for writes that must NOT silently fail: any non-2xx becomes an
/// `Err` carrying the status + body.
pub async fn ds_post_ok<B: ClientRequest>(state: &Arc<AppState>, body: &B) -> Result<()> {
    let resp = ds_post(state, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(DsRefused::error("ds_post", B::PATH, s, txt));
    }
    Ok(())
}

/// How an envelope post ended (#1041).
#[derive(Debug)]
pub enum EnvelopePost {
    Landed,
    /// Refused: the envelope's `(generation, epoch)` was not the commit log's
    /// head, so nothing is stored. The sender must re-seal at the head and retry.
    EpochBehind {
        head_generation: i64,
        head_epoch: i64,
    },
}

/// [`ds_post_ok`] for `/v1/messages/send` and `/v1/messages/edit`, returning the
/// DS's stale-epoch 409 as [`EnvelopePost::EpochBehind`]. A 409 whose body is not
/// an `EpochBehind` is an `Err`.
pub async fn ds_post_envelope<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<EnvelopePost> {
    let resp = ds_post(state, body).await?;
    let status = resp.status();
    if status.is_success() {
        return Ok(EnvelopePost::Landed);
    }
    let path = B::PATH;
    let txt = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::CONFLICT {
        if let Ok(behind) = serde_json::from_str::<pollis_api::messages::EpochBehind>(&txt) {
            if behind.error == pollis_api::messages::EpochBehind::ERROR {
                return Ok(EnvelopePost::EpochBehind {
                    head_generation: behind.head_generation,
                    head_epoch: behind.head_epoch,
                });
            }
        }
    }
    Err(DsRefused::error("ds_post", path, status, txt))
}

/// [`ds_post`] when this device can sign; otherwise fall back to the verified-OTP
/// session this sign-in minted. For account-lifecycle calls reachable before
/// enrollment (soft reset, Secret-Key recovery); the DS accepts either
/// (`gate_or_session`).
///
/// Both slots are tried: `bootstrap_session` (first-device signup) or
/// `enrollment_session` (re-login on a known device). For these calls each is a
/// bearer token proving the same freshly-verified mailbox.
pub async fn ds_post_signed_or_session<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    let can_sign = state.local_db.lock().await.is_some();
    if can_sign {
        return ds_post(state, body).await;
    }
    // Each guard is dropped before the next `.await`: a guard in a `match`
    // scrutinee would live across it. `state.rs`'s
    // `no_lock_guard_in_a_scrutinee_spans_an_await` enforces this.
    let bootstrap = state.bootstrap_session.lock().await.clone();
    let enrollment = state.enrollment_session.lock().await.clone();
    let token = bootstrap.or(enrollment).ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "not signed in and no verified-email session — verify your email again, then retry"
        ))
    })?;
    ds_post_session(state, &token, body).await
}

/// [`ds_post_signed_or_session`] for writes that must NOT silently fail: any
/// non-2xx becomes an `Err` carrying the status + body.
pub async fn ds_post_signed_or_session_ok<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<()> {
    let resp = ds_post_signed_or_session(state, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(DsRefused::error("ds_post", B::PATH, s, txt));
    }
    Ok(())
}

/// Report this device's applied MLS `since` epoch for a conversation
/// (`POST /v1/commits/since`); the DS retention floor is the MIN across current
/// members (#539). A truthful report can only protect its own history.
///
/// Device-SIGNED: it raises the retention floor, so the values live in the signed
/// body (#681). `generation` scopes it to one suite lineage (#454 P4).
///
/// Best-effort and event-driven: every failure is swallowed. [`REPORT_TIMEOUT`]
/// bounds the whole round trip here because neither `ds_post` nor the shared
/// client sets a timeout, and `migrate.rs` awaits this inline.
pub async fn ds_report_commit_since(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    generation: i64,
    since: i64,
) {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return,
    };
    let body = pollis_api::commit::CommitSinceReport {
        conversation_id: conversation_id.to_string(),
        generation,
        since,
        user_id: Some(user_id.to_string()),
        device_id: Some(device_id),
    };
    let _ = tokio::time::timeout(REPORT_TIMEOUT, ds_post(state, &body)).await;
}

/// Upper bound on a single best-effort high-water report's round trip.
const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolve the DS base URL, trailing slash trimmed, or error if it isn't
/// configured.
fn delivery_base(state: &Arc<AppState>) -> Result<&str> {
    state
        .config
        .pollis_delivery_url
        .as_deref()
        .map(|s| s.trim_end_matches('/'))
        .ok_or_else(|| Error::Other(anyhow::anyhow!("pollis_delivery_url not configured")))
}

/// Checked by the dev Worker (`pollis-delivery/worker/dev-gate.ts`).
pub(crate) const DEV_KEY_HEADER: &str = "X-Pollis-Dev-Key";

/// Attach the dev DS gate key (#1242) when the configured DS is not prod. Every
/// DS request builder in this file calls it; see
/// [`crate::config::Config::dev_access_key_for_ds`] for the host scoping.
fn with_dev_key(state: &AppState, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match state.config.dev_access_key_for_ds() {
        Some(key) => request.header(DEV_KEY_HEADER, key),
        None => request,
    }
}

/// [`send_ds`], plus ONE resend for an idempotent request whose first attempt
/// died on the connection (#1497, RFC 9110 §9.2.2), typically a stale pooled
/// keep-alive. Same signature, still inside the replay window. A non-idempotent
/// request is never resent: the first attempt may have landed.
async fn send_ds_resending(
    path: &str,
    request: reqwest::RequestBuilder,
    idempotent: bool,
) -> reqwest::Result<reqwest::Response> {
    let resend = if idempotent { request.try_clone() } else { None };
    match (send_ds(path, request).await, resend) {
        (Err(e), Some(again)) if is_connection_failure(&e) => {
            crate::diag!("[ds] {path}: resending once after a connection failure");
            send_ds(path, again).await
        }
        (result, _) => result,
    }
}

/// A transport failure on the CONNECTION (could not open, or died before an
/// answer). Not a timeout: a timed-out request may still be running on the DS.
pub(crate) fn is_connection_failure(e: &reqwest::Error) -> bool {
    !e.is_timeout() && (e.is_connect() || e.is_request())
}

/// An error and every `source()` beneath it, `": "`-joined — what a transport
/// failure actually was, not just reqwest's top line.
pub(crate) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

fn new_request_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Send a DS request stamped with a fresh
/// [`pollis_api::redact::REQUEST_ID_HEADER`] (#1305), logging any non-2xx or
/// transport failure with that id so it matches the DS's failed-request log.
async fn send_ds(path: &str, request: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    let request_id = new_request_id();
    let result = request.header(pollis_api::redact::REQUEST_ID_HEADER, &request_id).send().await;
    match &result {
        Ok(resp) if !resp.status().is_success() => {
            crate::diag!("[ds] {path} → {} req={request_id}", resp.status());
        }
        Err(e) => {
            // reqwest's Display omits the cause; it is in the source chain (#1497).
            crate::diag!(
                "[ds] {path} → transport error req={request_id}: {}",
                error_chain(e)
            );
        }
        Ok(_) => {}
    }
    result
}

/// POST `body` with NO auth headers, for the pre-identity OTP endpoints, which
/// the DS gates by the OTP itself. Returns the raw [`reqwest::Response`].
pub async fn ds_post_plain<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_plain serialize: {e}")))?;
    ds_post_plain_bytes(state, B::PATH, body_bytes).await
}

async fn ds_post_plain_bytes(
    state: &Arc<AppState>,
    path: &str,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response> {
    let url = format!("{}{}", delivery_base(state)?, path);
    let overlay = state.overlay_handle();
    let request = with_dev_key(state, crate::net::overlay::http_client(overlay.as_deref()).post(&url))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    send_ds(path, request)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_plain {path}: {e}")))
}

/// [`ds_post`] for the OTP-session-gated bootstrap writes: the device has no
/// signing key yet, so these carry the session token in `X-Pollis-Session`.
/// Returns the raw [`reqwest::Response`].
pub async fn ds_post_session<B: ClientRequest>(
    state: &Arc<AppState>,
    session_token: &str,
    body: &B,
) -> Result<reqwest::Response> {
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_session serialize: {e}")))?;
    ds_post_session_bytes(state, B::PATH, session_token, body_bytes).await
}

async fn ds_post_session_bytes(
    state: &Arc<AppState>,
    path: &str,
    session_token: &str,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response> {
    let url = format!("{}{}", delivery_base(state)?, path);
    let overlay = state.overlay_handle();
    let request = with_dev_key(state, crate::net::overlay::http_client(overlay.as_deref()).post(&url))
        .header("X-Pollis-Session", session_token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    send_ds(path, request)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_session {path}: {e}")))
}

/// [`ds_post_session`] for bootstrap writes that must NOT silently fail.
pub async fn ds_post_session_ok<B: ClientRequest>(
    state: &Arc<AppState>,
    session_token: &str,
    body: &B,
) -> Result<()> {
    let resp = ds_post_session(state, session_token, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let path = B::PATH;
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ds_post_session {path} {s}: {txt}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod dev_key_tests {
    use super::*;
    use crate::config::Config;
    use crate::keystore::{InMemoryKeystore, Keystore};
    use std::sync::Mutex as StdMutex;

    /// A DS that records the `X-Pollis-Dev-Key` of every request it gets
    /// (`None` when the header is absent) and answers 200.
    async fn recording_ds() -> (String, Arc<StdMutex<Vec<Option<String>>>>) {
        use axum::{http::HeaderMap, routing::post, Router};
        let seen: Arc<StdMutex<Vec<Option<String>>>> = Arc::default();
        let log = seen.clone();
        let app = Router::new().fallback(post(move |headers: HeaderMap| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(
                    headers
                        .get(DEV_KEY_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string),
                );
                "{}"
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    fn state(ds: String, key: Option<&str>) -> Arc<AppState> {
        let mut config = Config::for_test().unwrap();
        config.pollis_delivery_url = Some(ds);
        config.dev_access_key = key.map(str::to_string);
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        Arc::new(AppState::new_with_parts(config, keystore))
    }

    /// Both builders carry the key to a non-prod DS, and send no header when no
    /// key is configured.
    #[tokio::test]
    async fn the_ds_builders_send_the_dev_key_only_when_configured() {
        let (ds, seen) = recording_ds().await;
        let body = pollis_api::otp::RequestOtpBody { email: "a@example.com".into() };

        let keyed = state(ds.clone(), Some("dev-key"));
        ds_post_plain(&keyed, &body).await.unwrap();
        ds_post_session(&keyed, "session", &body).await.unwrap();

        let unkeyed = state(ds, None);
        ds_post_plain(&unkeyed, &body).await.unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![Some("dev-key".to_string()), Some("dev-key".to_string()), None]
        );
    }

    /// Every DS request builder goes through `with_dev_key`; a new one that
    /// skips it would silently skip the dev gate.
    #[test]
    fn every_ds_request_builder_attaches_the_dev_key() {
        // Built with concat! so these needles do not match this test's own source.
        let src = include_str!("ds_client.rs");
        let calls = src.matches(concat!("http_client", "(overlay")).count();
        let wrapped = src
            .matches(concat!("with_dev_key(state, crate::net::overlay::http_client", "(overlay"))
            .count();
        assert_eq!(calls, wrapped, "a DS request builder skips with_dev_key");
        assert_eq!(wrapped, 3);
        assert_eq!(src.matches(concat!("send_ds", "(path, request)")).count(), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DS that accepts the connection but never answers must not hang
    /// `ds_report_commit_since`: `REPORT_TIMEOUT` bounds the round trip.
    #[tokio::test]
    async fn report_round_trip_is_bounded_against_a_hanging_ds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });

        let client = crate::net::overlay::http_client(None);
        let url = format!("http://{addr}/v1/commits/since");

        // Control: the target is truly hanging, not refusing fast.
        let quick = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client.post(&url).body("{}").send(),
        )
        .await;
        assert!(
            quick.is_err(),
            "hanging DS should not resolve in 200ms — got {quick:?}"
        );

        let start = std::time::Instant::now();
        let bounded = tokio::time::timeout(
            REPORT_TIMEOUT,
            client.post(&url).body("{}").send(),
        )
        .await;
        let elapsed = start.elapsed();
        assert!(
            bounded.is_err(),
            "REPORT_TIMEOUT must bound a hanging DS round-trip, got {bounded:?}"
        );
        assert!(
            elapsed < REPORT_TIMEOUT * 2,
            "bound should fire near REPORT_TIMEOUT ({REPORT_TIMEOUT:?}), took {elapsed:?}"
        );
    }

    /// A DS whose FIRST connection reads the request and closes without an
    /// answer (a stale keep-alive), and answers 200 on every later one.
    async fn drops_the_first_connection() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let n = count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let Ok(read) = sock.read(&mut chunk).await else { return };
                        if read == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..read]);
                        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            if buf.len() >= end + 4 + 2 {
                                break;
                            }
                        }
                    }
                    if n == 0 {
                        return;
                    }
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                        .await;
                });
            }
        });
        (format!("http://{addr}/v1/r2/presign"), accepted)
    }

    /// #1497: an idempotent request survives a connection that dies before
    /// answering — one resend, on a new connection.
    #[tokio::test]
    async fn an_idempotent_request_is_resent_once_after_a_dropped_connection() {
        let (url, accepted) = drops_the_first_connection().await;
        let client = crate::net::overlay::http_client(None);
        let resp = send_ds_resending("/v1/r2/presign", client.post(&url).body("{}"), true)
            .await
            .expect("the resend must succeed");
        assert!(resp.status().is_success());
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_non_idempotent_request_is_never_resent() {
        let (url, accepted) = drops_the_first_connection().await;
        let client = crate::net::overlay::http_client(None);
        let err = send_ds_resending("/v1/messages/send", client.post(&url).body("{}"), false)
            .await
            .expect_err("no resend, so the dropped connection is the answer");
        assert!(is_connection_failure(&err), "unexpected error kind: {err:?}");
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn the_vault_save_path_is_declared_idempotent() {
        use pollis_api::DsRequest;
        const {
            assert!(pollis_api::broker::R2PresignBody::IDEMPOTENT);
            assert!(pollis_api::messages::AttachmentRegisterBody::IDEMPOTENT);
            assert!(pollis_api::vault::SaveVaultMessageBody::IDEMPOTENT);
            assert!(pollis_api::vault::DeleteVaultMessageBody::IDEMPOTENT);
            assert!(!pollis_api::messages::SendMessageBody::IDEMPOTENT);
            assert!(!pollis_api::commit::SubmitBody::IDEMPOTENT);
        }
    }
}
