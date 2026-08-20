//! Signed HTTP client for the Delivery Service's authenticated write endpoints.
//!
//! Pollis has no server-side session/token system: the only credential that maps
//! to a `user_id` server-side is the device's stable PQ MLS signing key
//! (`user_device.mls_signature_pub_pq`). So every write the client routes through
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
//! | `X-Pollis-Signature`  | base64 (STANDARD) of the 2420-byte ML-DSA-44 sig |
//!
//! The signature is ML-DSA-44 over the canonical message
//! `{METHOD}\n{PATH}\n{TIMESTAMP}\n{lowercase hex sha256(body)}` — byte-for-byte
//! what `pollis_delivery::auth::canonical_message` reconstructs and verifies.
//! When the DS has auth disabled (`POLLIS_DS_REQUIRE_AUTH=false`) the headers are
//! simply ignored, so signing every request is harmless in that mode.

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
    // `hex::encode` is lowercase, unseparated — byte-identical to the loop this
    // and three DS helpers each used to hand-roll, and the crate already depends
    // on `hex` so there was never a dependency to avoid.
    let hex = hex::encode(hasher.finalize());

    format!("{method}\n{path}\n{timestamp}\n{hex}").into_bytes()
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
/// There is no `path` argument: the path is [`DsRequest::PATH`], a property of
/// `B`, so the caller cannot post a body to an endpoint that does not expect it
/// and cannot misspell the route. `B` is the SAME type `pollis-delivery` parses
/// the request into (both come from `pollis-api`), so a field this client omits,
/// renames or invents fails to compile instead of arriving as a silently-absent
/// JSON key — the bug shape that shipped four broken actor fields and cost #918.
///
/// Returns the raw [`reqwest::Response`] so callers map status codes themselves
/// (e.g. 409 → `LostRace` on the commit path).
pub async fn ds_post<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    // Serialize in the generic shim, sign and send in a NON-generic core: every
    // endpoint would otherwise monomorphize the whole signing + overlay path,
    // which the mobile builds pay for in binary size for no benefit.
    let body_bytes = serde_json::to_vec(body)
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post serialize: {e}")))?;
    ds_post_bytes(state, B::PATH, body_bytes).await
}

/// The signing + sending half of [`ds_post`], with the body already serialized.
/// Non-generic on purpose (see [`ds_post`]).
async fn ds_post_bytes(
    state: &Arc<AppState>,
    path: &str,
    body_bytes: Vec<u8>,
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

    // The exact bytes we hash MUST be the exact bytes we send — hence `Vec<u8>`
    // in, serialized once by the caller and never re-encoded here.
    // Signed: the DS checks this against its own clock and the skew
    // arithmetic must not wrap when a client is ahead.
    let timestamp = crate::util::now_unix() as i64;
    let message = canonical_message("POST", path, timestamp, &body_bytes);

    // Sign with the device's stable MLS signing key. The provider wraps a !Send
    // rusqlite connection, so all signing is confined to this scope which ends
    // (dropping the guard) before any await below.
    let signature_b64 = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("not signed in for DS request signing")))?;
        // The certified device identity's PQ half (#668). Both leaf keys are
        // covered by the device cert; request auth uses the ML-DSA-44 one so a
        // future break of Ed25519 cannot forge writes from captured traffic.
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

    let url = format!("{}{}", base.trim_end_matches('/'), path);
    // First-party DS write — route through the overlay when it is on (§14.2).
    let overlay = state.overlay_handle();
    let resp = crate::net::overlay::http_client(overlay.as_deref())
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
///
/// `ciphersuite` names the suite pool to claim from; the DS never returns a
/// package from another suite, so a suite the target cannot serve comes back as
/// `Ok(None)` rather than a silently downgraded package. Passed explicitly so
/// the choice is visible at the call site — and it is not always `CS_PQ`, even
/// since #669: a caller adding a device to a group that has not yet migrated
/// must claim in *that group's* suite or the add will be rejected.
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
    let device_id = state.device_id.lock().await.clone();
    // `user_id`/`device_id` are the DS's no-auth fallback (`broker::resolve_user`):
    // auth on → taken from the verified signature and these are ignored; auth off
    // → these ARE the identity, and a body without them is a 400. Naming them
    // grants nothing — `ds_post` signs as this very user.
    let body = pollis_api::broker::LivekitTokenBody {
        room: room.to_string(),
        kind: Some(kind.to_string()),
        user_id: Some(current_user_id(state).await?),
        device_id,
    };
    let parsed = ds_post_json(state, &body).await?;

    // Resolve the fallback HERE, not at the call sites. The DS's URL is
    // authoritative — it is the server that will accept this JWT — but every one
    // of the six callers used to destructure it as `_url` and dial the compiled-in
    // `config.livekit_url` instead, which baked the SFU address into every shipped
    // binary and made "move the SFU" a client-release problem. Returning an
    // already-resolved URL means a caller cannot reintroduce that bug by ignoring
    // a field, and there is exactly one place the precedence is written down.
    //
    // Empty is the documented self-host case: a DS with no `LIVEKIT_URL` set. That
    // degrades to the previous behaviour rather than dialing "".
    let url = resolve_livekit_url(&parsed.url, &state.config.livekit_url);
    Ok((parsed.token, url))
}

/// Precedence for which LiveKit address to dial: the DS's answer wins, the
/// compiled-in config is only a fallback.
///
/// Pulled out as a pure function so the rule is testable without a live DS — the
/// bug this replaces was invisible precisely because it lived inline at six call
/// sites with nothing asserting it.
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

    /// The whole point: a DS-supplied address must never be discarded in favour
    /// of the compiled-in one. This is the invalid state that shipped — the SFU
    /// address baked into every binary, unmovable without a client release.
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

/// Fan out a **content-free** control `payload` to a LiveKit `room` via the DS's
/// server-side `RoomService/SendData` (the admin secret stays server-side).
/// Replaces the on-device `make_admin_token` + Twirp POST. Best-effort — the DS
/// treats a room with no participants (404) as success.
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
/// Returns each participant already resolved to a user (#836): the DS holds the
/// per-room identity key, so it decrypts the opaque identities LiveKit reports
/// and attaches the user id and display name on the way back. Internal
/// participants and `view` clients are filtered server-side. Desktop-only —
/// mobile has no Rust-side voice roster (see `livekit_stub`).
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
///
/// Re-exported from `pollis-api` rather than declared here (#922). It used to be
/// a local `#[derive(Deserialize)]` mirror alongside an `IdentityResp` that
/// carried BOTH a `participants` and an `identities` field so one struct could
/// decode two endpoints — which is precisely the shape #922 exists to remove: it
/// decodes either response, so it also decodes the WRONG one, silently, as an
/// empty vec. Each endpoint now names its own response type.
#[cfg(feature = "media")]
pub use pollis_api::broker::ResolvedIdentity;

/// Ask the DS to resolve opaque LiveKit participant identities back to users
/// (#836).
///
/// Identities minted for a room are only meaningful under that room's key, which
/// lives on the DS and nowhere else — a per-room key shipped in a release binary
/// could be extracted, and one that resolved every room would hand the whole
/// social graph back. So a client that learns a peer identity from the LiveKit
/// event stream asks here.
///
/// `room` is the **logical** room (conversation id / `inbox-<user>` /
/// `call-<ulid>`), not the LiveKit pseudonym. Identities the DS cannot resolve
/// are simply absent from the result; callers leave those participants
/// unattributed rather than guessing.
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

/// [`ds_post`] plus decoding the ONE response body this endpoint declares (#922).
///
/// The mirror image of what `ds_post` does for the request. Callers used to
/// declare a private `#[derive(Deserialize)] struct Resp { … }` at each call
/// site and hand-match it against a `json!{}` literal in the DS — two lists,
/// nothing forcing them to agree, and a failure mode quieter than the request
/// side's: a field the server renames does not error here, it simply arrives
/// absent and becomes a default the caller cannot tell from a real answer.
///
/// `B::Response` is the SAME associated type `pollis-delivery` hands to
/// `writes::ok_response::<B>`, so reading a field the server does not send is a
/// compile error rather than a `None`.
///
/// Any non-2xx is an `Err` carrying the status and body — a caller that needs to
/// branch on a specific status (404 on key-package claim, 409 on commit submit)
/// uses [`ds_post`] and reads the status itself.
pub async fn ds_post_json<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<B::Response> {
    let resp = ds_post(state, body).await?;
    let status = resp.status();
    if !status.is_success() {
        let path = B::PATH;
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_post {path} {status}: {txt}")));
    }
    decode_response::<B>(resp).await
}

/// Decode a 2xx body into the endpoint's declared response type.
///
/// Split out of [`ds_post_json`] so the handful of callers that must inspect the
/// status FIRST (a 404 that means "skip this device", a 409 that means "lost the
/// race") still decode through the one declaration rather than re-deriving a
/// local mirror of it.
pub(crate) async fn decode_response<B: DsRequest>(
    resp: reqwest::Response,
) -> Result<B::Response> {
    let path = B::PATH;
    resp.json::<B::Response>()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post {path} decode: {e}")))
}

/// [`ds_post`] for writes that must NOT silently fail: any non-2xx becomes an
/// `Err` carrying the status + body. Use this when the direct-write path it
/// replaces propagated its error (`conn.execute(...).await?`). For best-effort
/// writes (the direct path logged and continued) call [`ds_post`] and log
/// instead.
pub async fn ds_post_ok<B: ClientRequest>(state: &Arc<AppState>, body: &B) -> Result<()> {
    let resp = ds_post(state, body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let path = B::PATH;
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_post {path} {s}: {txt}")));
    }
    Ok(())
}

/// [`ds_post`] when this device can sign (local DB open, device key enrolled);
/// otherwise fall back to whichever verified-OTP session this sign-in minted.
/// For the account-lifecycle calls reachable from a PRE-ENROLLMENT device — the
/// soft reset and the Secret-Key recovery offered on the login gate — where no
/// signing key exists yet and the user's authorization is the email OTP they
/// just verified. The DS accepts either credential on these endpoints
/// (`gate_or_session`).
///
/// BOTH session slots are tried, and which one is populated depends on the
/// branch `verify_otp` took:
///
///   * `bootstrap_session` — a FIRST-device signup.
///   * `enrollment_session` — a re-login on a device the account already knows.
///     `verify_otp` keeps this one separate on purpose (the subsequent-device
///     cert publish must not consume a session, because a sibling approval can
///     outlast the TTL), but for THESE calls the two are interchangeable: each
///     is a bearer token proving the same freshly-verified mailbox.
///
/// Trying only `bootstrap_session` is how Secret-Key recovery on a returning
/// device failed with "not signed in and no verified-email session" — that
/// device had an `enrollment_session` and nothing else.
pub async fn ds_post_signed_or_session<B: ClientRequest>(
    state: &Arc<AppState>,
    body: &B,
) -> Result<reqwest::Response> {
    let can_sign = state.local_db.lock().await.is_some();
    if can_sign {
        return ds_post(state, body).await;
    }
    // Each guard is bound to a `let` and dropped before the next `.await`: in
    // edition 2021 a guard taken in a `match` scrutinee lives to the end of the
    // whole statement, so the obvious `match state.bootstrap_session.lock()...`
    // would hold that lock across the `enrollment_session.lock().await` below.
    // `state.rs`'s `no_lock_guard_in_a_scrutinee_spans_an_await` fails the build
    // on exactly that shape.
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
        let path = B::PATH;
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("ds_post {path} {s}: {txt}")));
    }
    Ok(())
}

/// Report this device's applied MLS `since` epoch for a conversation to the DS
/// (`POST /v1/commits/since`), the signal the server-side retention floor is the
/// MIN of across current members (#539, I4 Tier 1). `since` is the client's
/// current local epoch — it still needs every commit `>= since`, so a truthful
/// report can only ever PROTECT its own history from pruning.
///
/// **Device-SIGNED** (via [`ds_post`]). The report is a write — it raises the
/// retention floor — so it must be authenticated as the device it claims to be.
/// It used to be an unauthenticated `GET /v1/commits/{conv}?...&user_id&device_id`,
/// which let anyone raise the floor for any device and, chained with the
/// unclamped prune floor, wipe a conversation's whole commit log (#681). The
/// reported `(conversation_id, generation, since)` now lives in the signed JSON
/// body, so the signature's `sha256(body)` binding authenticates the values
/// themselves — a signed empty-body GET with them in the query string could not.
/// `user_id`/`device_id` are included for the DS's no-auth (dev/test) path only;
/// when the DS enforces auth it derives both from the verified signature.
///
/// `generation` scopes that position to one suite lineage (#454 P4). The DS keeps
/// the pair monotone lexicographically, which is what stops a migrated device's
/// report — epoch 0 of generation `N + 1`, numerically *below* the retired
/// lineage's last epoch — from reading as a regression and being dropped.
///
/// Fully best-effort and EVENT-DRIVEN (fires once per catch-up, never polls): any
/// failure — no DS URL, not signed in yet, a network error — is swallowed,
/// leaving the floor conservatively low (Tier 2's hard cap still bounds storage).
///
/// A short [`REPORT_TIMEOUT`] keeps it off the catch-up critical path: the report
/// is best-effort by design, so it must NEVER block the caller — one call site is
/// `await`ed inline on the suite-migration path (`migrate.rs`), where a
/// black-holed DS or a stalled overlay relay would otherwise hang migration
/// indefinitely. The bound lives HERE, wrapping the whole signed round-trip
/// (`ds_post` sets no timeout, and neither does the shared reqwest client — its
/// no-timeout default is deliberate, so long-lived media/streaming requests are
/// never cut off), so no future call site can forget it.
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
    // Bound the whole signed round-trip. A timeout resolves to `Err(_)` and is
    // swallowed exactly like any other failure — the report is dropped and the
    // floor stays conservative.
    let _ = tokio::time::timeout(REPORT_TIMEOUT, ds_post(state, &body)).await;
}

/// Upper bound on a single best-effort high-water report's round-trip. Short by
/// design: it sits on the catch-up (and inline suite-migration) path, so it must
/// give up quickly rather than stall real work behind an unreachable DS.
const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    let overlay = state.overlay_handle();
    crate::net::overlay::http_client(overlay.as_deref())
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ds_post_plain {path}: {e}")))
}

/// Sign-free sibling of [`ds_post`] for the OTP-session-gated bootstrap writes
/// (establish-identity / register-device / publish-device-cert). During bootstrap
/// the device has no MLS signing key yet, so these carry the OTP-session bearer
/// token in `X-Pollis-Session` instead of the four `X-Pollis-*` signature headers.
/// Returns the raw [`reqwest::Response`] so the caller maps the status.
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
    let overlay = state.overlay_handle();
    crate::net::overlay::http_client(overlay.as_deref())
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
mod tests {
    use super::*;

    /// A hanging DS — one that accepts the TCP connection but never sends a
    /// response — must not hang the caller. `ds_report_commit_since` wraps its
    /// signed round-trip in [`REPORT_TIMEOUT`] precisely because the shared
    /// reqwest client (`net::overlay::http_client`) and `ds_post` both set NO
    /// timeout of their own, so without the wrap the request would wait forever
    /// — and one call site (`migrate.rs`) awaits the report INLINE on the
    /// suite-migration path, so an unreachable DS would freeze migration.
    ///
    /// This pins the exact bound the function applies: `timeout(REPORT_TIMEOUT,
    /// <hanging POST>)` returns `Err(Elapsed)` in about `REPORT_TIMEOUT`, rather
    /// than pending indefinitely. The control assertion proves the target is
    /// genuinely hanging (not instantly refused), so the treatment is really
    /// exercising the bound.
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

        // Control: 200ms in, the request has neither completed nor failed — the
        // target is truly hanging, so the treatment below exercises the bound
        // rather than a fast connection-refused.
        let quick = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client.post(&url).body("{}").send(),
        )
        .await;
        assert!(
            quick.is_err(),
            "hanging DS should not resolve in 200ms — got {quick:?}"
        );

        // Treatment: the exact bound ds_report_commit_since applies. It must
        // RETURN (as Err(Elapsed)) rather than hang, and do so around
        // REPORT_TIMEOUT, not far beyond it.
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
}
