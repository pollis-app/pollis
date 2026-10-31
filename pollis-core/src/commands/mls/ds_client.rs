//! Signed HTTP client for the Delivery Service's authenticated write endpoints.
//!
//! The only credential mapping to a `user_id` is the device's PQ MLS signing key, so every DS write is signed with it (`pollis_delivery::auth::verify_request`).
//!
//! Each request carries four headers:
//!
//! | Header                | Value                                            |
//! |-----------------------|--------------------------------------------------|
//! | `X-Pollis-User`       | current `users.id`                               |
//! | `X-Pollis-Device`     | current `user_device.device_id` ULID             |
//! | `X-Pollis-Timestamp`  | unix seconds, decimal ASCII                       |
//! | `X-Pollis-Signature`  | base64 (STANDARD) of the 2420-byte ML-DSA-44 sig |
//!
//! The signature is ML-DSA-44 over the canonical message
//! `{METHOD}\n{PATH}\n{TIMESTAMP}\n{lowercase hex sha256(body)}` — byte-for-byte
//! what `pollis_delivery::auth::canonical_message` reconstructs and verifies.

use std::sync::Arc;

use openmls::prelude::Ciphersuite;
use openmls_traits::signatures::Signer;
use sha2::{Digest, Sha256};

use pollis_api::{ClientRequest, DsRequest};

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
    let hex = hex::encode(hasher.finalize());

    format!("{method}\n{path}\n{timestamp}\n{hex}").into_bytes()
}


/// The signing user's id for THIS `AppState`.
///
/// Prefer this instance's session: co-located dev clients share `accounts.json`, whose `last_active_user` may be another client's (401).
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

/// Sign and POST a typed request body to the one endpoint that body addresses,
/// attaching the four `X-Pollis-*` auth headers.
///
/// No `path` argument: the path is [`DsRequest::PATH`], and `B` is the type the DS parses, so mismatches fail to compile (#918).
///
/// Returns the raw [`reqwest::Response`] so callers map status codes themselves
/// (e.g. 409 → `LostRace` on the commit path).
pub async fn ds_post<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    // Generic shim, non-generic core: saves mobile binary size.
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post serialize: {e}")))?;
    ds_post_bytes(state, B::PATH, B::IDEMPOTENT, body_bytes).await
}

/// The signing + sending half of [`ds_post`], with the body already serialized.
/// Non-generic on purpose (see [`ds_post`]).
async fn ds_post_bytes(
    state: &Arc<AppState>,
    path: &str,
    idempotent: bool,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response> {
    // Resolved before the identity lookups below so a missing URL still reports
    // as "not configured" rather than "not signed in".
    let base = delivery_base(state)?;
    refuse_if_outdated(state)?;

    let user_id = current_user_id(state).await?;
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("device_id not set for DS request signing")))?;

    // Fail fast for a device the DS already called revoked (#1338); bound to this exact device.
    if state.is_revoked_device(&user_id, &device_id) {
        return Err(Error::DeviceRevoked);
    }

    // Hash exactly the bytes we send.
    // Signed: the DS checks this against its own clock and the skew
    // arithmetic must not wrap when a client is ahead.
    let timestamp = crate::util::now_unix() as i64;
    let message = canonical_message("POST", path, timestamp, &body_bytes);

    // The provider wraps a !Send connection, so signing stays in this scope, before any await.
    let signature_b64 = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("not signed in for DS request signing")))?;
        // Request auth uses the device's ML-DSA-44 key (#668).
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
    // First-party DS write — route through the overlay when it is on (§14.2).
    let overlay = state.overlay_handle();
    let request = ds_headers(state, crate::net::overlay::http_client(overlay.as_deref()).post(&url))
        .header("X-Pollis-User", &user_id)
        .header("X-Pollis-Device", &device_id)
        .header("X-Pollis-Timestamp", timestamp.to_string())
        .header("X-Pollis-Signature", signature_b64)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    let resp = send_ds_resending(path, request, idempotent)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post {path}: {}", error_chain(&e))))?;
    let resp = screen_upgrade_required(state, path, resp).await?;
    if is_device_revoked_response(&resp) {
        if note_revoked_response(state, &user_id, &device_id).await {
            crate::diag!("[ds] {path}: this device was revoked from the account; signing out");
        }
        return Err(Error::DeviceRevoked);
    }
    Ok(resp)
}

/// Record a `device_revoked` answer (#1338) only while that device is still this process's; returns whether it newly recorded one.
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
/// `x-pollis-auth-error: device_revoked`. Read from the header so the body is
/// left for callers that map their own statuses.
fn is_device_revoked_response(resp: &reqwest::Response) -> bool {
    resp.status() == reqwest::StatusCode::UNAUTHORIZED
        && resp
            .headers()
            .get("x-pollis-auth-error")
            .and_then(|v| v.to_str().ok())
            == Some("device_revoked")
}

/// Claim one of `target_user_id`'s unclaimed key packages via the DS; `Ok(None)` on 404 (none left), any other non-2xx errors.
///
/// Device-signed via [`ds_post`]; any authenticated user may claim a peer's package.
///
/// `ciphersuite` is explicit: a not-yet-migrated group must claim in its own suite; the DS never downgrades.
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

/// Ask the DS to mint a LiveKit participant token for `room`; user and device come from the verified signer. Returns `(token, ws_url)`.
pub async fn ds_livekit_token(
    state: &Arc<AppState>,
    room: &str,
    kind: &str,
) -> Result<(String, String)> {
    let device_id = state.device_id.lock().await.clone();
    // No-auth fallback identity; `ds_post` signs as this very user, so it grants nothing.
    let body = pollis_api::broker::LivekitTokenBody {
        room: room.to_string(),
        kind: Some(kind.to_string()),
        user_id: Some(current_user_id(state).await?),
        device_id,
    };
    let parsed = ds_post_json(state, &body).await?;

    // Resolve the URL here so no caller can ignore the DS's address and dial the compiled-in one.
    //
    // Empty DS URL (self-host) falls back to config.
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

    /// Self-host case: a DS with no LIVEKIT_URL set must degrade to the
    /// compiled-in default, never to an empty dial.
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

    /// Both unset stays empty so the caller's "LiveKit is not configured" check
    /// still fires, rather than dialing a whitespace URL.
    #[test]
    fn both_empty_stays_empty() {
        assert_eq!(resolve_livekit_url("", ""), "");
    }
}

/// Fan out a **content-free** control `payload` to a LiveKit `room` via the DS's `SendData`; a 404 (empty room) is success.
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

/// List a voice room's roster via the DS (server-side `ListParticipants`).
///
/// Participants come back resolved to users (#836); desktop-only.
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

/// One participant identity resolved back to a Pollis user (#836).
pub use pollis_api::broker::ResolvedIdentity;

/// Ask the DS to resolve opaque LiveKit participant identities back to users
/// (#836).
///
/// Only the DS holds the per-room identity key, so clients ask here.
///
/// `room` is the **logical** room; unresolvable identities are simply absent.
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

/// [`ds_post`] plus decoding the ONE response body this endpoint declares (#922).
///
/// `B::Response` is the type the DS sends, so a missing field is a compile error.
///
/// Any non-2xx is an `Err` carrying the status and body — a caller that needs to
/// branch on a specific status (404 on key-package claim, 409 on commit submit)
/// uses [`ds_post`] and reads the status itself.
pub async fn ds_post_json<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<B::Response> {
    let resp = reject_non_2xx(ds_post(state, body).await?, B::PATH, "ds_post").await?;
    decode_response::<B>(resp).await
}

/// Pass a 2xx response through; turn anything else into an `Err` carrying the
/// status and the body text.
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

/// A non-2xx DS answer, typed so callers can `downcast_ref` the status; `Display` is unchanged (used by the outbox, #1335).
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

/// Decode a 2xx body into the endpoint's declared response type.
pub(crate) async fn decode_response<B: DsRequest>(
    resp: reqwest::Response,
) -> Result<B::Response> {
    let path = B::PATH;
    resp.json::<B::Response>()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post {path} decode: {e}")))
}

/// [`ds_post`] for writes that must NOT silently fail: any non-2xx is an `Err`.
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
    /// Stored.
    Landed,
    /// Refused: the envelope's epoch was not the log head; catch up, re-seal, re-post.
    EpochBehind {
        head_generation: i64,
        head_epoch: i64,
    },
}

/// [`ds_post_ok`] for message send/edit, returning a stale-epoch 409 as [`EnvelopePost::EpochBehind`]; any other non-2xx is an `Err`.
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

/// [`ds_post`] when this device can sign (local DB open, device key enrolled);
/// otherwise fall back to whichever verified-OTP session this sign-in minted.
/// For pre-enrollment account-lifecycle calls (soft reset, Secret-Key recovery); the DS accepts either (`gate_or_session`).
///
/// BOTH session slots are tried:
///
///   * `bootstrap_session` — a FIRST-device signup.
///   * `enrollment_session` — a re-login on a known device.
///
/// Trying only `bootstrap_session` broke Secret-Key recovery on a returning device.
pub async fn ds_post_signed_or_session<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    let can_sign = state.local_db.lock().await.is_some();
    if can_sign {
        return ds_post(state, body).await;
    }
    // Bind each guard to a `let` so it drops before the next await (`no_lock_guard_in_a_scrutinee_spans_an_await`).
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

/// [`ds_post_signed_or_session`] plus decoding the endpoint's declared response,
/// for the reads a not-yet-enrolled device also makes (#1616). Any non-2xx is an `Err`.
pub async fn ds_post_signed_or_session_json<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<B::Response> {
    let resp = reject_non_2xx(
        ds_post_signed_or_session(state, body).await?,
        B::PATH,
        "ds_post",
    )
    .await?;
    decode_response::<B>(resp).await
}

/// Report this device's applied MLS `since` epoch (`POST /v1/commits/since`); the retention floor is the MIN across members (#539).
///
/// **Device-SIGNED**: the report raises the floor, so its values ride in the signed body (#681).
///
/// `generation` keeps the pair lexicographically monotone across a migration (#454 P4).
///
/// Best-effort and event-driven; failures leave the floor conservatively low.
///
/// [`REPORT_TIMEOUT`] bounds the whole round-trip here, since `migrate.rs` awaits it inline and nothing else sets a timeout.
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
        // Consulted only on the DS's no-auth dev/test path; ignored (but still
        // signature-bound) when the DS enforces auth.
        user_id: Some(user_id.to_string()),
        device_id: Some(device_id),
    };
    // Bound the round-trip; a timeout is swallowed like any failure.
    let _ = tokio::time::timeout(REPORT_TIMEOUT, ds_post(state, &body)).await;
}

/// Upper bound on one best-effort report round-trip.
const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolve the DS base URL, trailing slash trimmed, or error if it isn't
/// configured. Shared by every sender: the signed [`ds_post_bytes`] above and the
/// unauthenticated + session-bearer bootstrap clients below.
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

/// The headers every DS request carries: the dev DS gate key (#1242) when the configured DS is
/// not prod, and this build's platform and version (#1611) when the host set them.
/// Every DS request goes through one of the three builders in this file, and
/// each calls this, so desktop, CLI and mobile all send them from one place; see
/// [`crate::config::Config::dev_access_key_for_ds`] for the host scoping.
fn ds_headers(state: &AppState, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let request = match &state.config.client {
        Some(client) => request
            .header(pollis_api::client_policy::CLIENT_PLATFORM_HEADER, client.platform.as_str())
            .header(pollis_api::client_policy::CLIENT_VERSION_HEADER, &client.version),
        None => request,
    };
    match state.config.dev_access_key_for_ds() {
        Some(key) => request.header(DEV_KEY_HEADER, key),
        None => request,
    }
}

/// Fail locally once this build is known to be below the minimum (#1611).
fn refuse_if_outdated(state: &AppState) -> Result<()> {
    crate::client_policy::refuse_if_blocked(state)
}

/// The DS's typed `426 Upgrade Required` (#1611) blocks this build from here on. A 426 without
/// that body is an ordinary refusal; anything else passes through.
async fn screen_upgrade_required(state: &AppState, path: &str, resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status() != reqwest::StatusCode::UPGRADE_REQUIRED {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    match crate::client_policy::record_upgrade_required(state, &body) {
        Some(blocked) => {
            crate::diag!(
                "[ds] {path}: the DS requires a newer build (minimum {})",
                blocked.minimum.as_deref().unwrap_or("unknown")
            );
            Err(Error::UpgradeRequired)
        }
        None => Err(DsRefused::error("ds", path, status, body)),
    }
}

/// [`send_ds`] plus ONE resend for an idempotent request whose connection died before answering (#1497); never for a non-idempotent one.
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

/// A failure on the CONNECTION (could not open, or died before answering); not a timeout, which may still be running on the DS.
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

/// 16 hex characters from the OS RNG.
fn new_request_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Send a DS request stamped with a fresh request id (#1305); non-2xx and transport failures are logged with it.
async fn send_ds(path: &str, request: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    let request_id = new_request_id();
    let result = request.header(pollis_api::redact::REQUEST_ID_HEADER, &request_id).send().await;
    match &result {
        Ok(resp) if !resp.status().is_success() => {
            crate::diag!("[ds] {path} → {} req={request_id}", resp.status());
        }
        Err(e) => {
            // Log the source chain: reqwest's Display omits the cause (#1497).
            crate::diag!(
                "[ds] {path} → transport error req={request_id}: {}",
                error_chain(e)
            );
        }
        Ok(_) => {}
    }
    result
}

/// POST with NO auth headers, for the pre-identity OTP endpoints the DS gates by the OTP itself.
pub async fn ds_post_plain<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_plain serialize: {e}")))?;
    ds_post_plain_bytes(state, B::PATH, body_bytes).await
}

/// Non-generic half of [`ds_post_plain`] — see [`ds_post`] for why.
async fn ds_post_plain_bytes(
    state: &Arc<AppState>,
    path: &str,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response> {
    let url = format!("{}{}", delivery_base(state)?, path);
    refuse_if_outdated(state)?;
    let overlay = state.overlay_handle();
    let request = ds_headers(state, crate::net::overlay::http_client(overlay.as_deref()).post(&url))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    let resp = send_ds(path, request)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_plain {path}: {e}")))?;
    screen_upgrade_required(state, path, resp).await
}

/// Sign-free sibling of [`ds_post`] for OTP-session-gated bootstrap writes (`X-Pollis-Session` bearer).
pub async fn ds_post_session<B: ClientRequest>(
    state: &Arc<AppState>,
    session_token: &str,
    body: &B,
) -> Result<reqwest::Response> {
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_session serialize: {e}")))?;
    ds_post_session_bytes(state, B::PATH, session_token, body_bytes).await
}

/// Non-generic half of [`ds_post_session`] — see [`ds_post`] for why.
async fn ds_post_session_bytes(
    state: &Arc<AppState>,
    path: &str,
    session_token: &str,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response> {
    let url = format!("{}{}", delivery_base(state)?, path);
    refuse_if_outdated(state)?;
    let overlay = state.overlay_handle();
    let request = ds_headers(state, crate::net::overlay::http_client(overlay.as_deref()).post(&url))
        .header("X-Pollis-Session", session_token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes);
    let resp = send_ds(path, request)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_session {path}: {e}")))?;
    screen_upgrade_required(state, path, resp).await
}

/// [`ds_post_session`] for bootstrap writes that must NOT silently fail: any
/// non-2xx becomes an `Err` carrying the status + body.
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

    /// The pre-credential and session builders send the key to a non-prod DS, and nothing when none is configured.
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

    /// Every DS request is built in this file, and every builder here goes
    /// through `ds_headers`. A new builder that calls `http_client` directly
    /// would silently skip the dev gate; this makes it a test failure.
    #[test]
    fn every_ds_request_builder_attaches_the_dev_key() {
        // Built with concat! so these needles do not match this test's own
        // source. A DS builder passes the live overlay; tests pass `None`.
        let src = include_str!("ds_client.rs");
        let calls = src.matches(concat!("http_client", "(overlay")).count();
        let wrapped = src
            .matches(concat!("ds_headers(state, crate::net::overlay::http_client", "(overlay"))
            .count();
        assert_eq!(calls, wrapped, "a DS request builder skips ds_headers");
        assert_eq!(wrapped, 3);
        // ...and every one of them sends through `send_ds`, which stamps the
        // request id (#1305).
        assert_eq!(src.matches(concat!("send_ds", "(path, request)")).count(), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hanging DS must not hang the caller: pins the [`REPORT_TIMEOUT`] bound `ds_report_commit_since` applies.
    #[tokio::test]
    async fn report_round_trip_is_bounded_against_a_hanging_ds() {
        // Accept connections and hold them open forever without replying.
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

        // Control: still pending at 200ms, so the target truly hangs.
        let quick = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client.post(&url).body("{}").send(),
        )
        .await;
        assert!(
            quick.is_err(),
            "hanging DS should not resolve in 200ms — got {quick:?}"
        );

        // Treatment: returns Err(Elapsed) around REPORT_TIMEOUT.
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
    /// answer — what a client sees when it reuses a keep-alive connection the
    /// far end had already dropped — and answers 200 on every later one.
    /// Returns its URL and how many connections it has accepted.
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
                    // Read until the end of the headers plus the 2-byte body.
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
                        // Close with no answer at all.
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

    /// The other half: a request that may NOT be repeated is never resent, so
    /// the connection failure reaches the caller.
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

    /// The endpoints a Vault save is made of are the ones declared repeatable;
    /// a message send is not.
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

#[cfg(test)]
mod client_policy_tests {
    use super::*;
    use crate::client_policy::{ClientIdentity, PolicySource};
    use crate::config::Config;
    use crate::keystore::{InMemoryKeystore, Keystore};
    use pollis_api::client_policy::{ClientPlatform, CLIENT_PLATFORM_HEADER, CLIENT_VERSION_HEADER};
    use std::sync::Mutex as StdMutex;

    type Seen = Arc<StdMutex<Vec<(Option<String>, Option<String>)>>>;

    /// A DS that records the client headers and answers `status` with `body`.
    async fn ds(status: u16, body: &'static str) -> (String, Seen) {
        use axum::{http::HeaderMap, http::StatusCode, routing::post, Router};
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = Router::new().fallback(post(move |headers: HeaderMap| {
            let log = log.clone();
            async move {
                let get = |h: &str| headers.get(h).and_then(|v| v.to_str().ok()).map(str::to_string);
                log.lock().unwrap().push((get(CLIENT_PLATFORM_HEADER), get(CLIENT_VERSION_HEADER)));
                (StatusCode::from_u16(status).unwrap(), body)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    fn state(ds: String, client: Option<ClientIdentity>) -> Arc<AppState> {
        let mut config = Config::for_test().unwrap();
        config.pollis_delivery_url = Some(ds);
        config.client = client;
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        Arc::new(AppState::new_with_parts(config, keystore))
    }

    fn body() -> pollis_api::otp::RequestOtpBody {
        pollis_api::otp::RequestOtpBody { email: "a@example.com".into() }
    }

    #[tokio::test]
    async fn every_request_names_the_build_when_the_host_set_it() {
        let (url, seen) = ds(200, "{}").await;
        let mac = ClientIdentity { platform: ClientPlatform::Macos, version: "1.17.0".into() };
        ds_post_plain(&state(url.clone(), Some(mac)), &body()).await.unwrap();
        ds_post_session(&state(url.clone(), Some(ClientIdentity::cli())), "s", &body()).await.unwrap();
        // A build that does not say (tests, older hosts) sends nothing, and the DS lets it pass.
        ds_post_plain(&state(url, None), &body()).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (Some("macos".into()), Some("1.17.0".into())),
                (Some("cli".into()), Some(env!("CARGO_PKG_VERSION").into())),
                (None, None),
            ]
        );
    }

    const UPGRADE: &str = r#"{"error":"upgrade_required","platform":"ios","version":"1.0.3","minimum":"1.0.21","message":"Please update.","policy":{"v":1,"minimum":{"ios":"1.0.21"},"message":"Please update."}}"#;

    /// A 426 blocks the build: that call and every later one fail with `upgrade_required`, the
    /// later ones without reaching the DS.
    #[tokio::test]
    async fn a_426_blocks_the_build_from_then_on() {
        let (url, seen) = ds(426, UPGRADE).await;
        let ios = ClientIdentity { platform: ClientPlatform::Ios, version: "1.0.3".into() };
        let state = state(url, Some(ios));
        let err = ds_post_plain(&state, &body()).await.unwrap_err();
        assert_eq!(err.to_string(), "upgrade_required");
        let status = crate::client_policy::current(&state);
        assert!(status.blocked);
        assert_eq!(status.source, PolicySource::Ds);
        assert_eq!(status.minimum.as_deref(), Some("1.0.21"));
        assert_eq!(status.message.as_deref(), Some("Please update."));

        let err = ds_post_session(&state, "s", &body()).await.unwrap_err();
        assert_eq!(err.to_string(), "upgrade_required");
        assert_eq!(seen.lock().unwrap().len(), 1, "a blocked build no longer reaches the DS");
    }

    /// Only the DS's typed body blocks: a proxy's or edge's 426 is an ordinary error.
    #[tokio::test]
    async fn an_untyped_426_is_an_ordinary_error() {
        for reply in ["nope", r#"{"error":"something_else","platform":"cli","version":"1.0.0","minimum":"2.0.0","policy":{"v":1}}"#] {
            let (url, seen) = ds(426, reply).await;
            let state = state(url, Some(ClientIdentity::cli()));
            let err = ds_post_plain(&state, &body()).await.unwrap_err().to_string();
            assert_ne!(err, "upgrade_required", "{reply}");
            assert!(err.contains("426"), "{err}");
            assert!(!crate::client_policy::current(&state).blocked, "{reply}");
            // Not latched: the next request still reaches the DS.
            let _ = ds_post_plain(&state, &body()).await;
            assert_eq!(seen.lock().unwrap().len(), 2, "{reply}");
        }
    }
}
