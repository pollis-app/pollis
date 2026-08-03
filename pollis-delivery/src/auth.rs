//! Device-certificate-signature authentication for write requests.
//!
//! Pollis has **no server-side session/token system**. The only server-side
//! credential that maps to a `user_id` is the device's MLS signing key:
//! `user_device.mls_signature_pub_pq` — the device's raw 1312-byte ML-DSA-44
//! public key, one of the two leaf keys wrapped in `device_cert` and verified by
//! pollis-core's `verify_device_cert`. So DS auth reuses that cross-signing
//! device identity: the client **signs each write with its ML-DSA-44 device
//! private key**, and the DS verifies the signature against the registered
//! public key. No new token table, no shared secret at rest.
//!
//! Request auth is post-quantum (#668) because a harvest-now-decrypt-later
//! adversary who breaks Ed25519 later could otherwise forge writes against the
//! captured `mls_signature_pub`. The classic key stays in `mls_signature_pub`
//! for the classic MLS suite's leaves — it is simply no longer an auth
//! credential.
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
//! | `X-Pollis-Signature`  | base64 (STANDARD) of the 2420-byte ML-DSA-44 sig |
//!
//! The signature header is ~3228 base64 characters. That is well inside every
//! default header-size limit on the path (hyper 16 KiB/header, Cloudflare 16
//! KiB total) but it is the reason no proxy in front of the DS may be
//! configured below an 8 KiB header budget.
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
//! The signature is **ML-DSA-44** (deterministic, hedged variant disabled) over
//! that message, produced by the device's PQ MLS signing private key; the
//! verifying key is the raw 1312-byte `mls_signature_pub_pq` stored in
//! `user_device` — no length prefix, no TLS wrapper (that is exactly what
//! openmls `SignatureKeyPair::to_public_vec()` returns for the `MLDSA44`
//! signature scheme, and what pollis-core's `verify_device_cert` consumes).
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
//!
//! ## Device-pubkey cache (#658)
//!
//! [`lookup_device_pubkey`] was the ONE thing every device-signed endpoint did
//! that the fast unsigned bootstrap endpoints did not: a Turso round trip on
//! EVERY request. From the Cloudflare Container's network position that query
//! alone cost 2000–4700 ms server-side, and a first message in a fresh mobile
//! group chains ~14 sequential signed calls — 15–60 s of user-visible latency.
//! Signature verification itself is sub-millisecond, so the fix is to stop
//! re-reading an almost-never-changing row.
//!
//! [`DeviceKeyCache`] holds `(user_id, device_id) -> verifying key` in process
//! memory. Three properties make it safe:
//!
//!   1. **Every mutation of `user_device` evicts.** Revoke, logout, resign,
//!      identity rotation, account delete, reset-recover, device registration and
//!      the cert publish all call [`DeviceKeyCache::invalidate_device`] /
//!      [`DeviceKeyCache::invalidate_user`]. See the type docs for the full list.
//!   2. **A short absolute TTL** ([`DEVICE_KEY_CACHE_TTL_SECS`]) backstops a
//!      missed eviction, so a stale entry can never live indefinitely.
//!   3. **Positive entries only.** A miss is never cached — see
//!      [`DeviceKeyCache`] for why.
//!
//! The cache is only ever consulted AFTER [`parse_credentials`] has passed
//! (headers present, timestamp inside the replay window, signature well-formed),
//! and it only ever supplies the *verifying key* — the ML-DSA-44 signature is
//! still checked in full on every single request. A cache hit therefore removes
//! a DB read, never a cryptographic check.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use libsql::Connection;
use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa44, Verifier};
use sha2::{Digest, Sha256};

use pollis_device_cert::{MLDSA44_PUB_LEN, MLDSA44_SIG_LEN};

type Signature = ml_dsa::Signature<MlDsa44>;
type VerifyingKey = ml_dsa::VerifyingKey<MlDsa44>;

use crate::error::AuthRejection;

/// Replay window, in seconds, on either side of the server clock. See module
/// docs for the tradeoff.
pub const REPLAY_WINDOW_SECS: i64 = 300;

/// How long a cached device pubkey may be served before it is re-read from
/// Turso, in seconds — measured from INSERTION, never refreshed on use.
///
/// **This is a backstop, not the invalidation mechanism.** Every endpoint that
/// mutates `user_device` evicts explicitly (see [`DeviceKeyCache`]); the TTL only
/// bounds the damage if some future endpoint forgets to. 30s is chosen because:
///
///   - it is the *maximum* window in which a revoked/logged-out device could
///     still authenticate if an eviction hook were ever missed, and 30s of
///     residual access is recoverable (the MLS reconcile that actually removes
///     the device from each group is a separate client-side commit anyway);
///   - it is far shorter than the ±300s [`REPLAY_WINDOW_SECS`] already accepted
///     on this path, so the cache is not the weakest link in the auth story;
///   - it still absorbs essentially all of #658's damage: the pathological case
///     is a burst of ~14 sequential signed calls, which completes well inside
///     30s once the first call has warmed the entry.
///
/// The TTL is ABSOLUTE (from insertion), not sliding. A sliding TTL would let a
/// continuously-active device keep a stale entry alive forever, which is exactly
/// the attacker profile the backstop exists for.
pub const DEVICE_KEY_CACHE_TTL_SECS: i64 = 30;

/// Soft cap on live cache entries. Entries are only created for devices that
/// really exist in `user_device`, so this can only be reached by a genuinely
/// enormous fleet — it exists so a pathological case degrades into extra DB
/// reads rather than unbounded memory. Overflow clears the map, which is always
/// safe (clearing can only cause a re-read, never an acceptance).
const DEVICE_KEY_CACHE_MAX_ENTRIES: usize = 10_000;

/// One cached verifying key. `cached_at` is the server unix-seconds clock that
/// was passed to [`verify_request_identity_cached`] when the entry was inserted.
struct CachedKey {
    key: Arc<VerifyingKey>,
    cached_at: i64,
}

/// In-process cache of `(user_id, device_id) -> mls_signature_pub_pq` (#658).
///
/// **Single instance by construction.** The DS deploys with `max_instances: 1`
/// (`pollis-delivery/wrangler.{dev,prod}.jsonc`) — one container, enforced for
/// the sole-writer invariant — so there is exactly ONE process and ONE cache.
/// That is what makes plain eviction sufficient: there is no second instance
/// holding a copy we cannot reach. **If `max_instances` is ever raised, this
/// cache becomes incoherent across instances**: instance A would evict on
/// revoke while instance B kept serving the revoked device's key for up to
/// [`DEVICE_KEY_CACHE_TTL_SECS`]. Raising `max_instances` therefore requires
/// either dropping this cache or replacing eviction with a shared signal
/// (pub/sub, or a cheap `revoked_at` watermark read).
///
/// **Negative results are deliberately NOT cached.** A missing/NULL-pubkey
/// `user_device` row is the *normal* state of a device mid-enrollment: the
/// bootstrap sequence registers the row and then publishes the cert moments
/// later, and a cached "no such device" would 401 a device that has since become
/// valid — breaking device registration for up to a TTL. Caching negatives would
/// also let an unauthenticated caller grow the map with arbitrary
/// header-supplied `(user, device)` pairs. Since a miss already resolves to 401,
/// not caching it costs one Turso read on a path that is failing anyway, and
/// keeps the cache's entry set bounded by the real device fleet.
///
/// **Eviction points** — every DS write that can change what
/// [`lookup_device_pubkey`] would return:
///
/// | Endpoint                        | `user_device` effect          | Call            |
/// |---------------------------------|-------------------------------|-----------------|
/// | `POST /v1/devices/revoke`       | `UPDATE … SET revoked_at`     | device          |
/// | `POST /v1/auth/logout`          | `DELETE` own row              | device          |
/// | `POST /v1/devices/resign`       | `UPDATE` cert columns         | user            |
/// | `POST /v1/account/rotate-identity` | new account identity       | user            |
/// | `POST /v1/account/delete`       | `DELETE users` → FK cascade   | user            |
/// | `POST /v1/account/reset-recover`| `DELETE` other/all devices    | user            |
/// | `POST /v1/auth/register-device` | `INSERT` / upsert row         | device          |
/// | `POST /v1/auth/publish-device-cert` | `UPDATE … mls_signature_pub_pq` | device  |
///
/// The last two are the ones that can *change a live pubkey* rather than remove
/// one, so they matter even though nothing is being revoked.
///
/// **Known race, bounded by the TTL.** A request that reads the row just before
/// a revoke commits can insert its (now stale) entry just after that revoke's
/// eviction, resurrecting it for up to [`DEVICE_KEY_CACHE_TTL_SECS`]. Closing it
/// fully needs a per-key generation counter shared with the writer; the TTL is
/// the deliberate, documented bound instead. The window is one request's
/// DB-read-to-insert span, and both sides run in the same process.
///
/// `Clone` is shallow (shared `Arc`), so it rides on the `Clone` `AppState`
/// exactly like [`crate::session::SessionStore`].
#[derive(Clone, Default)]
pub struct DeviceKeyCache {
    inner: Arc<Mutex<HashMap<(String, String), CachedKey>>>,
}

impl DeviceKeyCache {
    /// The cached key for `(user_id, device_id)`, or `None` on a miss or an
    /// entry older than [`DEVICE_KEY_CACHE_TTL_SECS`].
    ///
    /// An entry whose `cached_at` is in the FUTURE relative to `now` (the server
    /// clock jumped backwards) is treated as stale, not as fresh — fail closed.
    fn get(&self, user_id: &str, device_id: &str, now: i64) -> Option<Arc<VerifyingKey>> {
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        let key = (user_id.to_string(), device_id.to_string());
        let age = match guard.get(&key) {
            Some(entry) => now - entry.cached_at,
            None => return None,
        };
        if (0..DEVICE_KEY_CACHE_TTL_SECS).contains(&age) {
            return guard.get(&key).map(|e| Arc::clone(&e.key));
        }
        // Expired (or clock went backwards) — drop it so it can't be served.
        guard.remove(&key);
        None
    }

    /// Record a freshly-read, LIVE (non-revoked, well-formed) pubkey. Only
    /// called on a positive DB lookup — see the type docs on negatives.
    fn insert(&self, user_id: &str, device_id: &str, key: Arc<VerifyingKey>, now: i64) {
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        if guard.len() >= DEVICE_KEY_CACHE_MAX_ENTRIES {
            guard.retain(|_, e| (0..DEVICE_KEY_CACHE_TTL_SECS).contains(&(now - e.cached_at)));
            if guard.len() >= DEVICE_KEY_CACHE_MAX_ENTRIES {
                guard.clear();
            }
        }
        guard.insert(
            (user_id.to_string(), device_id.to_string()),
            CachedKey { key, cached_at: now },
        );
    }

    /// Evict ONE device. Call after any write that revokes, deletes, or
    /// re-keys that specific `user_device` row.
    pub fn invalidate_device(&self, user_id: &str, device_id: &str) {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .remove(&(user_id.to_string(), device_id.to_string()));
    }

    /// Evict EVERY device of one user. Call after account-scoped writes whose
    /// blast radius is the whole fleet (identity rotation, account delete,
    /// reset-recover, cert re-sign) — cheaper to reason about than enumerating
    /// the affected device ids, and never less safe.
    pub fn invalidate_user(&self, user_id: &str) {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .retain(|(u, _), _| u != user_id);
    }

    /// Drop everything. Not used on the request path; exposed for operational
    /// escape hatches and tests.
    pub fn clear(&self) {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .clear();
    }

    /// Live entry count, including not-yet-swept expired entries. Test/telemetry
    /// observability only — never an auth input.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .len()
    }

    /// `true` when nothing is cached. Present because clippy insists a `len` has
    /// one.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const H_USER: &str = "x-pollis-user";
const H_DEVICE: &str = "x-pollis-device";
const H_TIMESTAMP: &str = "x-pollis-timestamp";
pub(crate) const H_SIGNATURE: &str = "x-pollis-signature";

/// The four headers parsed off an authenticated request, plus the
/// authenticated identity once the signature checks out.
struct Credentials {
    user_id: String,
    device_id: String,
    timestamp: i64,
    /// 2420-byte ML-DSA-44 signature, already base64-decoded.
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
    if sig_bytes.len() != MLDSA44_SIG_LEN {
        return Err(AuthRejection::Unauthorized);
    }
    let encoded: &EncodedSignature<MlDsa44> = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| AuthRejection::Unauthorized)?;
    let signature = Signature::decode(encoded).ok_or(AuthRejection::Unauthorized)?;

    Ok(Credentials {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        timestamp,
        signature,
    })
}

/// Look up the registered `mls_signature_pub_pq` for `(user_id, device_id)`.
///
/// Returns `Ok(Some(pub))` for a live, enrolled device; `Ok(None)` if the row
/// is absent, revoked, or has a NULL/wrong-length pubkey — all of which the
/// caller treats as "unknown device" → 401. A NULL column specifically means a
/// device that has not published a cert since #668; it must re-run
/// `publish-device-cert` (which is session/cert gated, never device-signature
/// gated, so it is always reachable) before it can authenticate a write. A DB
/// error propagates as `Err` and the caller still rejects (never fails open).
async fn lookup_device_pubkey(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Option<VerifyingKey>> {
    let mut rows = conn
        .query(
            "SELECT mls_signature_pub_pq, revoked_at \
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

    if pub_bytes.len() != MLDSA44_PUB_LEN {
        return Ok(None);
    }
    let encoded: &EncodedVerifyingKey<MlDsa44> = match pub_bytes.as_slice().try_into() {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    Ok(Some(VerifyingKey::decode(encoded)))
}

/// Full verification of a write request.
///
/// On success returns the authenticated `user_id` so the caller can bind it to
/// `body.sender_id`. The steps, in order, each rejecting on failure:
///   1. all four headers present, timestamp in window, signature decodes;
///   2. the device's `mls_signature_pub_pq` exists, is live (not revoked) and is
///      exactly [`MLDSA44_PUB_LEN`] bytes;
///   3. the ML-DSA-44 signature verifies over the canonical message.
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
    Ok(verify_request_identity(conn, headers, method, path, body, now)
        .await?
        .0)
}

/// [`verify_request`], served from `cache` when the device's pubkey is already
/// there (#658). Identical semantics — the ML-DSA-44 signature is still verified
/// in full; only the Turso read for the verifying key is skipped.
pub async fn verify_request_cached(
    cache: &DeviceKeyCache,
    conn: &Connection,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<String, AuthRejection> {
    Ok(
        verify_request_identity_cached(cache, conn, headers, method, path, body, now)
            .await?
            .0,
    )
}

/// [`verify_request`] returning BOTH halves of the authenticated identity —
/// `(user_id, device_id)`. Handlers that record something keyed on the specific
/// DEVICE (e.g. the retention high-water in `record_commit_since`, #681) need the
/// authenticated `device_id`, not just the user, and must take it from the
/// VERIFIED signature rather than from an unauthenticated request field. Both
/// values are the exact ones the signature was checked against — the pubkey was
/// looked up by `(user_id, device_id)` and a revoked/unknown device never gets
/// past [`lookup_device_pubkey`].
pub async fn verify_request_identity(
    conn: &Connection,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<(String, String), AuthRejection> {
    verify_request_inner(None, conn, headers, method, path, body, now).await
}

/// [`verify_request_identity`], served from `cache` on a hit (#658). Same
/// contract, same rejections — only the pubkey READ is elided, never the
/// signature check.
pub async fn verify_request_identity_cached(
    cache: &DeviceKeyCache,
    conn: &Connection,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<(String, String), AuthRejection> {
    verify_request_inner(Some(cache), conn, headers, method, path, body, now).await
}

/// The one implementation of the verify path. `cache: None` is the uncached
/// behaviour (every lookup hits the DB) — used by the in-process integration
/// harnesses, which build their own routers and have no `AppState` to hang a
/// cache off.
async fn verify_request_inner(
    cache: Option<&DeviceKeyCache>,
    conn: &Connection,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<(String, String), AuthRejection> {
    // Header/timestamp/signature-shape checks first: nothing reaches the cache
    // or the DB until the request is at least well-formed and in-window.
    let creds = parse_credentials(headers, now)?;

    let verifying_key = match cache.and_then(|c| c.get(&creds.user_id, &creds.device_id, now)) {
        Some(vk) => vk,
        None => {
            let vk = match lookup_device_pubkey(conn, &creds.user_id, &creds.device_id).await {
                Ok(Some(vk)) => Arc::new(vk),
                // Unknown / revoked device, or a DB error: never fail open, and
                // never cache the negative (see `DeviceKeyCache` docs).
                Ok(None) | Err(_) => return Err(AuthRejection::Unauthorized),
            };
            if let Some(c) = cache {
                c.insert(&creds.user_id, &creds.device_id, Arc::clone(&vk), now);
            }
            vk
        }
    };

    let message = canonical_message(method, path, creds.timestamp, body);
    verifying_key
        .verify(&message, &creds.signature)
        .map_err(|_| AuthRejection::Unauthorized)?;

    Ok((creds.user_id, creds.device_id))
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
