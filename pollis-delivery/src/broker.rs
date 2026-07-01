//! Authorized-secrets broker (#393).
//!
//! Two operations still done on-device hold a long-lived secret in the client
//! bundle: minting a LiveKit access token (needs the LiveKit API secret) and
//! reaching R2 (needs the R2 access key + secret). Shipping those secrets in
//! the client is the whole problem — anyone who unpacks the app can extract
//! them. This module moves both server-side: the DS holds the secrets in its
//! env, the (already device-signed) client asks the DS to mint a token / presign
//! a URL, and the secrets never leave the server.
//!
//! Both endpoints reuse the existing device-signature auth ([`crate::auth`] via
//! [`crate::writes::gate`]) — no new auth scheme. The point of server-side
//! minting is precisely that the **identity is derived from the verified
//! signer, not from anything the client sends**: a client cannot mint a LiveKit
//! token as another user.
//!
//! ## Why R2 presign needs no per-object authz
//!
//! Pollis media is **convergent-encrypted** (see `pollis-core`'s `r2.rs`):
//! the AES-256-GCM key is derived from `SHA-256(plaintext)`, and the
//! `attachment_object` table is a **global content-hash dedup** with no
//! conversation binding at all. A presigned URL therefore only ever exposes
//! **ciphertext** — confidentiality comes from MLS key distribution (only a
//! member who decrypted the message learns the content hash, and only the
//! content hash derives the decryption key), NOT from the R2 ACL. So the
//! presign gate exists solely to stop **anonymous internet access** to the
//! bucket; it does not — and cannot meaningfully — enforce read authz on a
//! per-object basis. Requiring an authenticated device is the right and
//! sufficient gate.
//!
//! ## Contract
//!
//! This module's request/response shapes ARE the contract the frontend `bridge`
//! (and mobile, via uniffi) will call once the on-device LiveKit/R2 paths are
//! removed (that client cutover is the follow-up to #393). See
//! `docs/secrets-broker.md`.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use libsql::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate, is_member, ok_json, Authed};
use crate::AppState;

// ── Config ───────────────────────────────────────────────────────────────────

/// Secrets the broker needs, read from DS env in [`BrokerConfig::from_env`]. All
/// `Option` — a missing secret makes the matching endpoint return 503 (the
/// endpoint still exists and answers, mirroring OTP with no Resend key) rather
/// than failing at startup. Default is all-`None` (no broker configured), so the
/// integration harness and unconfigured deploys keep working.
#[derive(Clone, Default)]
pub struct BrokerConfig {
    /// LiveKit API key — the JWT `iss` claim (env `LIVEKIT_API_KEY`).
    pub livekit_api_key: Option<String>,
    /// LiveKit API secret — the HS256 signing key (env `LIVEKIT_API_SECRET`).
    /// NEVER logged.
    pub livekit_api_secret: Option<String>,
    /// LiveKit ws URL handed back to the client (env `LIVEKIT_URL`).
    pub livekit_url: Option<String>,
    /// R2 S3 endpoint, e.g. `https://<acct>.r2.cloudflarestorage.com`
    /// (env `R2_ENDPOINT`).
    pub r2_endpoint: Option<String>,
    /// R2 region — SigV4 scope; defaults to `auto` (env `R2_REGION`).
    pub r2_region: String,
    /// R2 bucket name (env `R2_BUCKET`).
    pub r2_bucket: Option<String>,
    /// R2 access key id (env `R2_ACCESS_KEY_ID`).
    pub r2_access_key_id: Option<String>,
    /// R2 secret access key — SigV4 signing secret (env `R2_SECRET_ACCESS_KEY`).
    /// NEVER logged.
    pub r2_secret_access_key: Option<String>,
}

impl BrokerConfig {
    /// Read every broker secret from the DS environment. Empty strings are
    /// treated as unset. `R2_REGION` defaults to `auto` (Cloudflare R2's region).
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Self {
            livekit_api_key: var("LIVEKIT_API_KEY"),
            livekit_api_secret: var("LIVEKIT_API_SECRET"),
            livekit_url: var("LIVEKIT_URL"),
            r2_endpoint: var("R2_ENDPOINT"),
            r2_region: var("R2_REGION").unwrap_or_else(|| "auto".to_string()),
            r2_bucket: var("R2_BUCKET"),
            r2_access_key_id: var("R2_ACCESS_KEY_ID"),
            r2_secret_access_key: var("R2_SECRET_ACCESS_KEY"),
        }
    }

    /// All three LiveKit fields present → the token endpoint can sign.
    fn livekit_ready(&self) -> Option<(&str, &str, &str)> {
        Some((
            self.livekit_api_key.as_deref()?,
            self.livekit_api_secret.as_deref()?,
            self.livekit_url.as_deref()?,
        ))
    }

    /// All R2 fields present → the presign endpoint can sign.
    fn r2_ready(&self) -> Option<(&str, &str, &str, &str)> {
        Some((
            self.r2_endpoint.as_deref()?,
            self.r2_bucket.as_deref()?,
            self.r2_access_key_id.as_deref()?,
            self.r2_secret_access_key.as_deref()?,
        ))
    }
}

/// Resolve the user the broker acts as.
///
///   - auth ON  → the verified signer; any client-supplied identity is ignored
///                (the whole point — a signed request can only act as itself).
///   - auth OFF → the body's `user_id` (no signed identity on the no-auth path).
///                Missing/empty → 400. Mirrors [`crate::writes`]' resolvers.
fn resolve_user(authed: &Authed, body_user_id: Option<&str>) -> Result<String, Response> {
    match authed {
        Some(u) => Ok(u.clone()),
        None => match body_user_id {
            Some(b) if !b.is_empty() => Ok(b.to_string()),
            _ => Err(bad_request("user_id required when auth is disabled")),
        },
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn not_configured(what: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": format!("{what} broker not configured") })),
    )
        .into_response()
}

// ── 1. POST /v1/livekit/token ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LivekitTokenBody {
    /// The LiveKit room to mint a token for.
    pub room: String,
    /// `true` → the screenshare `:view` participant variant (identity suffixed
    /// `:view`, `canPublishData=false`). Default `false`.
    #[serde(default)]
    pub view: bool,
    /// No-auth path only: the user to mint for. IGNORED when auth is enforced
    /// (the identity comes from the verified signer there).
    #[serde(default)]
    pub user_id: Option<String>,
}

/// LiveKit JWT claims — byte-identical to pollis-core's `livekit_jwt::make_token`
/// so a token minted here is indistinguishable from the (soon-removed) on-device
/// one to the LiveKit SFU.
#[derive(Serialize)]
struct LiveKitClaims {
    iss: String,
    sub: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    name: String,
    video: VideoGrants,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoGrants {
    room: String,
    room_join: bool,
    can_publish: bool,
    can_subscribe: bool,
    can_publish_data: bool,
}

/// POST /v1/livekit/token — mint a LiveKit access token for the authenticated
/// user. Identity + display name are derived SERVER-SIDE from the verified
/// signer (a client cannot mint a token as someone else). Authorizes the room:
/// the user's own inbox room (`inbox-<user_id>`) is always allowed; any other
/// room requires current membership.
pub async fn livekit_token(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    let parsed: LivekitTokenBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if parsed.room.trim().is_empty() {
        return Ok(bad_request("room required"));
    }

    // Secrets gate: a clear 503 when the broker isn't configured, just like OTP
    // with no Resend key.
    let (api_key, api_secret, url) = match state.broker.livekit_ready() {
        Some(t) => t,
        None => return Ok(not_configured("livekit")),
    };

    let user_id = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    // Room authz — only on the signed path (mirrors the other handlers, which
    // skip authz when auth is disabled). The user's own inbox room is always
    // allowed; everything else demands current membership.
    if authed.is_some() {
        let inbox = format!("inbox-{user_id}");
        if parsed.room != inbox {
            let conn = state.db.conn()?;
            if !is_member(&conn, &parsed.room, &user_id).await? {
                return Ok(AuthRejection::Forbidden.into_response());
            }
        }
    }

    // Display name = the user's username (LiveKit `name`), looked up server-side.
    // Falls back to the user_id when the row is absent (no-auth path / unknown).
    let display_name = {
        let conn = state.db.conn()?;
        lookup_username(&conn, &user_id).await?.unwrap_or_else(|| user_id.clone())
    };

    // `:view` is a hidden screenshare publisher — identity suffixed, no data
    // channel (mirrors pollis-core's `make_view_token`).
    let identity = if parsed.view {
        format!("{user_id}:view")
    } else {
        user_id.clone()
    };

    let token = sign_livekit_token(
        api_key,
        api_secret,
        &parsed.room,
        &identity,
        &display_name,
        !parsed.view,
        now_unix(),
    )?;

    Ok(ok_json(serde_json::json!({ "token": token, "url": url })))
}

/// Look up a user's username for the LiveKit display name.
async fn lookup_username(conn: &Connection, user_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT username FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<String>(0).ok()),
        None => Ok(None),
    }
}

/// Sign an HS256 LiveKit JWT. `can_publish_data` is `false` for the `:view`
/// variant. `now` is injected so the claim times are testable. Pure (no I/O) so
/// it's directly unit-testable.
pub fn sign_livekit_token(
    api_key: &str,
    api_secret: &str,
    room: &str,
    identity: &str,
    display_name: &str,
    can_publish_data: bool,
    now: u64,
) -> anyhow::Result<String> {
    let claims = LiveKitClaims {
        iss: api_key.to_string(),
        sub: identity.to_string(),
        iat: now,
        nbf: now,
        exp: now + 3600,
        name: display_name.to_string(),
        video: VideoGrants {
            room: room.to_string(),
            room_join: true,
            can_publish: true,
            can_subscribe: true,
            can_publish_data,
        },
    };
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(api_secret.as_bytes());
    Ok(encode(&header, &claims, &key)?)
}

// ── 2. POST /v1/r2/presign ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct R2PresignBody {
    /// `"get"` → presign a GET (download); `"put"` → presign a PUT (upload).
    pub operation: String,
    /// The R2 object key (within the bucket), e.g. `media/<hash>/<file>.enc`.
    pub key: String,
    /// Optional content type — accepted for forward-compat; the presigned URL
    /// signs only `host`, so the client sets Content-Type at upload time.
    #[serde(default)]
    pub content_type: Option<String>,
    /// No-auth path only — see [`resolve_user`]. Unused beyond the auth gate
    /// (presign has no per-object authz), kept for shape-symmetry with the other
    /// broker endpoint.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// Default presigned-URL lifetime, in seconds.
const PRESIGN_EXPIRES_SECS: u64 = 900;

/// POST /v1/r2/presign — return a SigV4 presigned URL for a GET or PUT against
/// the configured R2 bucket. Requires an authenticated device (when auth is
/// enforced, [`gate`] rejects an unsigned request with 401). There is NO
/// per-object conversation check — see the module docs: the bucket holds only
/// convergently-encrypted ciphertext, so the gate exists to stop anonymous
/// access, not to enforce read authz.
pub async fn r2_presign(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    let parsed: R2PresignBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    let http_method = match parsed.operation.as_str() {
        "get" => "GET",
        "put" => "PUT",
        _ => return Ok(bad_request("operation must be \"get\" or \"put\"")),
    };
    if parsed.key.trim().is_empty() {
        return Ok(bad_request("key required"));
    }

    let (endpoint, bucket, access_key, secret_key) = match state.broker.r2_ready() {
        Some(t) => t,
        None => return Ok(not_configured("r2")),
    };

    // On the no-auth path there's no signed identity; the auth gate already
    // enforced presence when `require_auth` is on. Resolve only to validate the
    // no-auth body shape (and reject an empty/absent user_id there).
    if let Err(resp) = resolve_user(&authed, parsed.user_id.as_deref()) {
        return Ok(resp);
    }

    let url = presign_r2_url(
        endpoint,
        bucket,
        &state.broker.r2_region,
        access_key,
        secret_key,
        http_method,
        &parsed.key,
        PRESIGN_EXPIRES_SECS,
        &amz_datetime(),
    );

    Ok(ok_json(serde_json::json!({
        "url": url,
        "method": http_method,
        "expires_in": PRESIGN_EXPIRES_SECS,
    })))
}

// ── SigV4 query-string presign ───────────────────────────────────────────────
//
// The query-string ("presigned URL") variant of AWS SigV4, ported from the
// auth-header form in pollis-core's `r2.rs`. Single-chunk, `UNSIGNED-PAYLOAD`,
// `host` the only signed header. The five `X-Amz-*` params go in the canonical
// query string; the signature is appended last (it is never itself signed).

/// Compute the current UTC time as the SigV4 `YYYYMMDDTHHMMSSZ` basic-format
/// timestamp. Split out so the handler stays I/O-only and the pure
/// [`presign_r2_url`] takes the timestamp as an argument (testable).
fn amz_datetime() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Build a SigV4 presigned URL for `method` on `bucket/key`. Pure — `datetime`
/// is injected — so tests can pin the clock and reproduce the signature.
#[allow(clippy::too_many_arguments)]
pub fn presign_r2_url(
    endpoint: &str,
    bucket: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    method: &str,
    key: &str,
    expires: u64,
    datetime: &str,
) -> String {
    let date = &datetime[..8];
    let host = host_of(endpoint);

    // Canonical URI: `/<bucket>/<key>`, each path segment URI-encoded but with
    // `/` preserved (S3 encodes paths exactly once).
    let canonical_uri = format!(
        "/{}/{}",
        uri_encode(bucket, false),
        uri_encode(key, false)
    );

    let credential = format!("{access_key}/{date}/{region}/s3/aws4_request");
    // Canonical query: params sorted by name, values URI-encoded (the credential
    // `/`s become %2F). X-Amz-Signature is NOT part of the canonical query.
    let canonical_query = {
        let mut params = [
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", uri_encode(&credential, true)),
            ("X-Amz-Date", datetime.to_string()),
            ("X-Amz-Expires", expires.to_string()),
            ("X-Amz-SignedHeaders", "host".to_string()),
        ];
        params.sort_by(|a, b| a.0.cmp(b.0));
        params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    };

    let canonical_headers = format!("host:{host}\n");
    let signed_headers = "host";
    let payload_hash = "UNSIGNED-PAYLOAD";
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(secret_key, date, region, "s3");
    let signature = hex_lower(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "{}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}",
        scheme_host(endpoint)
    )
}

/// `https://host` (no path) of an endpoint URL, for building the final URL.
fn scheme_host(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("https", url),
    };
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{scheme}://{host}")
}

/// Bare host (no scheme, no path) — the SigV4 `host` header value.
fn host_of(url: &str) -> &str {
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    rest.split('/').next().unwrap_or(rest)
}

/// AWS-style percent-encoding (RFC 3986). Unreserved chars pass through; when
/// `encode_slash` is false, `/` is preserved (used for path segments). Matches
/// the canonical encoding S3 SigV4 requires.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (b == b'/' && !encode_slash);
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_lower(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_secret = format!("AWS4{secret}");
    let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Lowercase hex, no separators. (pollis-delivery deliberately avoids the `hex`
/// crate — `auth.rs` has the same helper.)
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
