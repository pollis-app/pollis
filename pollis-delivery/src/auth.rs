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
//! memory. Four properties make it safe:
//!
//!   1. **Every mutation of `user_device` evicts.** Revoke, logout, resign,
//!      identity rotation, account delete, reset-recover, device registration and
//!      the cert publish all call [`DeviceKeyCache::invalidate_device`] /
//!      [`DeviceKeyCache::invalidate_user`]. See the type docs for the full list.
//!   2. **A per-key generation counter closes the read-then-evict race (#721).**
//!      A miss captures the generation before it reads the row; an invalidation
//!      bumps it; an insert whose generation is stale is discarded, so a revoke
//!      that commits during an in-flight read can never be resurrected. See the
//!      type docs.
//!   3. **A short absolute TTL** ([`DEVICE_KEY_CACHE_TTL_SECS`]) backstops a
//!      missed eviction hook, so a stale entry can never live indefinitely.
//!   4. **Positive entries only.** A miss is never cached — see
//!      [`DeviceKeyCache`] for why.
//!
//! The cache is only ever consulted AFTER [`parse_credentials`] has passed
//! (headers present, timestamp inside the replay window, signature well-formed),
//! and it only ever supplies the *verifying key* — the ML-DSA-44 signature is
//! still checked in full on every single request. A cache hit therefore removes
//! a DB read, never a cryptographic check.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
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

/// One slot in the cache map for a `(user_id, device_id)`.
///
/// A slot outlives its cached key. An invalidation drops `key` to `None` but
/// bumps and KEEPS `generation`, because an in-flight read that already captured
/// the old generation must still be told, at insert time, that the world moved
/// under it (see [`DeviceKeyCache`] on the read-then-evict race). A slot with
/// `key: None` is therefore pure generation-tracking state — a tombstone — and is
/// swept by the same TTL as a real entry so tombstones cannot accumulate.
struct Slot {
    /// The cached verifying key, or `None` for a bare generation tombstone.
    key: Option<Arc<VerifyingKey>>,
    /// Per-key monotonic generation, bumped by every [`DeviceKeyCache::invalidate_device`]
    /// of this key. Read-captured before the DB read; an insert is discarded
    /// unless it is unchanged.
    generation: u64,
    /// Server unix-seconds when this slot was last written — a key insert OR a
    /// generation bump. TTL is measured from here.
    cached_at: i64,
}

/// The generation stamp a cache MISS captures, so the matching insert can detect
/// a write that raced between the read and the insert. `key` is the per-key
/// generation; `bulk` is the process-wide user-scoped generation (see
/// [`DeviceKeyCache`]). An insert is admitted only if BOTH still match.
#[derive(Clone, Copy)]
struct GenStamp {
    key: u64,
    bulk: u64,
}

/// The outcome of consulting the cache: a live key, or a miss carrying the
/// generation stamp the follow-up insert must present.
enum Lookup {
    Hit(Arc<VerifyingKey>),
    Miss(GenStamp),
}

/// A test-only async barrier invoked on the cache-miss path AFTER the device row
/// has been read and BEFORE the freshly-read key is inserted — the exact window
/// of the read-then-evict race (#721). Production never installs one
/// (`miss_barrier` stays `None`, a single `Option` check on the miss path); a
/// test installs one to deterministically slot a concurrent revoke into that
/// window. Boxed so it can capture the test's synchronisation state.
type MissBarrier =
    Arc<dyn Fn(&str, &str) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The mutable state behind the cache's single `Mutex`.
#[derive(Default)]
struct Inner {
    map: HashMap<(String, String), Slot>,
    /// Process-wide generation bumped by every [`DeviceKeyCache::invalidate_user`]
    /// (and [`DeviceKeyCache::clear`]). A read captures it alongside the per-key
    /// generation; a user-scoped invalidation between a read and its insert — rare
    /// (account rotate/delete/reset, cert re-sign) — discards the insert. It is
    /// deliberately coarse: user-scoped writes are infrequent, so the cost of
    /// discarding an unrelated in-flight insert is at most one extra DB read, and
    /// in exchange it closes the race for a device whose user is bulk-invalidated
    /// even when that device was never individually cached.
    bulk_generation: u64,
    /// See [`MissBarrier`]. Always `None` outside tests.
    miss_barrier: Option<MissBarrier>,
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
/// **The read-then-evict race is closed by a per-key generation counter (#721).**
/// A request that read the row just before a revoke committed could formerly
/// insert its now-stale entry just after that revoke's eviction, resurrecting a
/// revoked key for up to [`DEVICE_KEY_CACHE_TTL_SECS`]. The fix: every miss
/// captures the key's [`GenStamp`] BEFORE it reads the DB row; every invalidation
/// bumps that generation; an insert whose stamp no longer matches is discarded
/// rather than written. Because the capture strictly precedes the DB read, any
/// invalidation that commits after the read is guaranteed to have bumped the
/// generation past the captured value, so the racing insert always loses. The
/// per-key generation covers [`invalidate_device`](Self::invalidate_device); a
/// process-wide `bulk_generation` (see [`Inner`]) covers the coarser
/// [`invalidate_user`](Self::invalidate_user). The TTL now backstops nothing but
/// a genuinely missed eviction hook.
///
/// `Clone` is shallow (shared `Arc`), so it rides on the `Clone` `AppState`
/// exactly like [`crate::session::SessionStore`].
#[derive(Clone, Default)]
pub struct DeviceKeyCache {
    inner: Arc<Mutex<Inner>>,
}

impl DeviceKeyCache {
    /// Consult the cache for `(user_id, device_id)`.
    ///
    /// A live, in-TTL key is a [`Lookup::Hit`]. Anything else — no slot, a bare
    /// generation tombstone, or an expired slot — is a [`Lookup::Miss`] carrying
    /// the [`GenStamp`] the follow-up [`insert`](Self::insert) must present. The
    /// stamp is captured HERE, under the lock, before the caller reads the DB row:
    /// that ordering is what makes a later invalidation always observable at
    /// insert time.
    ///
    /// An entry whose `cached_at` is in the FUTURE relative to `now` (the server
    /// clock jumped backwards) is treated as stale, not as fresh — fail closed.
    fn lookup(&self, user_id: &str, device_id: &str, now: i64) -> Lookup {
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        let bulk = guard.bulk_generation;
        let key = (user_id.to_string(), device_id.to_string());
        match guard.map.get(&key) {
            Some(slot) if (0..DEVICE_KEY_CACHE_TTL_SECS).contains(&(now - slot.cached_at)) => {
                match &slot.key {
                    Some(vk) => Lookup::Hit(Arc::clone(vk)),
                    None => Lookup::Miss(GenStamp {
                        key: slot.generation,
                        bulk,
                    }),
                }
            }
            // Expired (or clock went backwards): drop the slot so nothing stale is
            // served, and treat its generation as a fresh line (0). A concurrent
            // invalidation re-creates a tombstone at generation 1, which still
            // beats this miss's captured 0 at insert time.
            Some(_) => {
                guard.map.remove(&key);
                Lookup::Miss(GenStamp { key: 0, bulk })
            }
            None => Lookup::Miss(GenStamp { key: 0, bulk }),
        }
    }

    /// The test-only miss barrier, cloned out so it can be awaited WITHOUT holding
    /// the cache lock. `None` in production.
    fn miss_barrier(&self) -> Option<MissBarrier> {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .miss_barrier
            .clone()
    }

    /// Record a freshly-read, LIVE (non-revoked, well-formed) pubkey — UNLESS the
    /// `stamp` captured at the matching miss is now stale, i.e. an invalidation
    /// (device- or user-scoped) committed between the read and here. In that case
    /// the insert is discarded: the key we hold may be the one that was just
    /// revoked, so it must not be resurrected. Only ever called on a positive DB
    /// lookup — see the type docs on negatives.
    fn insert(&self, user_id: &str, device_id: &str, key: Arc<VerifyingKey>, now: i64, stamp: GenStamp) {
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        // A user-scoped invalidation raced us.
        if guard.bulk_generation != stamp.bulk {
            return;
        }
        let map_key = (user_id.to_string(), device_id.to_string());
        // A device-scoped invalidation raced us (an absent slot reads as
        // generation 0, matching a miss that captured 0).
        let current_gen = guard.map.get(&map_key).map(|s| s.generation).unwrap_or(0);
        if current_gen != stamp.key {
            return;
        }
        if guard.map.len() >= DEVICE_KEY_CACHE_MAX_ENTRIES {
            guard
                .map
                .retain(|_, s| (0..DEVICE_KEY_CACHE_TTL_SECS).contains(&(now - s.cached_at)));
            if guard.map.len() >= DEVICE_KEY_CACHE_MAX_ENTRIES {
                guard.map.clear();
            }
        }
        guard.map.insert(
            map_key,
            Slot {
                key: Some(key),
                generation: current_gen,
                cached_at: now,
            },
        );
    }

    /// Evict ONE device. Call after any write that revokes, deletes, or re-keys
    /// that specific `user_device` row. Bumps the key's generation and leaves a
    /// tombstone even when nothing was cached, so a concurrent read that already
    /// captured the old generation cannot resurrect the key it read (#721).
    pub fn invalidate_device(&self, user_id: &str, device_id: &str) {
        let now = now_unix();
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        let slot = guard
            .map
            .entry((user_id.to_string(), device_id.to_string()))
            .or_insert(Slot {
                key: None,
                generation: 0,
                cached_at: now,
            });
        slot.key = None;
        slot.generation += 1;
        slot.cached_at = now;
    }

    /// Evict EVERY device of one user. Call after account-scoped writes whose
    /// blast radius is the whole fleet (identity rotation, account delete,
    /// reset-recover, cert re-sign) — cheaper to reason about than enumerating
    /// the affected device ids, and never less safe. Bumps the process-wide
    /// `bulk_generation` so an in-flight read for ANY of the user's devices — even
    /// one that was never individually cached — loses its insert (#721).
    pub fn invalidate_user(&self, user_id: &str) {
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        guard.bulk_generation = guard.bulk_generation.wrapping_add(1);
        guard.map.retain(|(u, _), _| u != user_id);
    }

    /// Drop everything. Not used on the request path; exposed for operational
    /// escape hatches and tests. Bumps `bulk_generation` too, so an insert whose
    /// stamp was captured before the clear cannot repopulate behind it.
    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("device key cache mutex poisoned");
        guard.bulk_generation = guard.bulk_generation.wrapping_add(1);
        guard.map.clear();
    }

    /// Live cached-key count — bare generation tombstones (`key: None`) are NOT
    /// counted, so eviction still reads as "empty". Includes not-yet-swept expired
    /// keys. Test/telemetry observability only — never an auth input.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .map
            .values()
            .filter(|s| s.key.is_some())
            .count()
    }

    /// `true` when no live key is cached. Present because clippy insists a `len`
    /// has one.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Install the test-only miss barrier (see [`MissBarrier`]). Test infra: never
    /// called in production. Named distinctly from the private accessor so the
    /// public surface reads as "set".
    pub fn set_miss_barrier(&self, barrier: MissBarrier) {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .miss_barrier = Some(barrier);
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

    let verifying_key = match cache {
        // Cached path (the shipping DS). A miss captures the key's generation
        // stamp BEFORE the DB read, and the matching insert is discarded if an
        // invalidation bumped it in between (#721).
        Some(c) => match c.lookup(&creds.user_id, &creds.device_id, now) {
            Lookup::Hit(vk) => vk,
            Lookup::Miss(stamp) => {
                let vk = match lookup_device_pubkey(conn, &creds.user_id, &creds.device_id).await {
                    Ok(Some(vk)) => Arc::new(vk),
                    // Unknown / revoked device, or a DB error: never fail open,
                    // and never cache the negative (see `DeviceKeyCache` docs).
                    Ok(None) | Err(_) => return Err(AuthRejection::Unauthorized),
                };
                // Test seam (#721): with a barrier installed, park here — after
                // the read, before the insert — so a test can commit a revoke into
                // the exact race window. A no-op in production.
                if let Some(barrier) = c.miss_barrier() {
                    barrier(&creds.user_id, &creds.device_id).await;
                }
                c.insert(&creds.user_id, &creds.device_id, Arc::clone(&vk), now, stamp);
                vk
            }
        },
        // Uncached path: the in-process integration harnesses build their own
        // routers and have no `AppState` to hang a cache off — every lookup hits
        // the DB.
        None => match lookup_device_pubkey(conn, &creds.user_id, &creds.device_id).await {
            Ok(Some(vk)) => Arc::new(vk),
            Ok(None) | Err(_) => return Err(AuthRejection::Unauthorized),
        },
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

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// A well-formed (but never verified) verifying key for populating the cache.
    fn dummy_key() -> Arc<VerifyingKey> {
        let bytes = [0u8; MLDSA44_PUB_LEN];
        let encoded: &EncodedVerifyingKey<MlDsa44> = bytes.as_slice().try_into().unwrap();
        Arc::new(VerifyingKey::decode(encoded))
    }

    /// Lock-contention microbenchmark for the cache's `Mutex<HashMap>` (#721 DoD
    /// item 3): hammer the hot read path from 1..=16 threads and report ns/op and
    /// throughput, so the single mutex can be judged against a realistic request
    /// rate before anyone reaches for sharding or an `RwLock`.
    ///
    /// Ignored by default (it spins CPU for a second). Run it explicitly:
    ///   cargo test -p pollis-delivery --lib bench::mutex_contention -- --ignored --nocapture
    #[test]
    #[ignore = "microbenchmark; run with --ignored --nocapture"]
    fn mutex_contention() {
        const DEVICES: usize = 256;
        const OPS_PER_THREAD: usize = 200_000;

        let cache = DeviceKeyCache::default();
        let now = now_unix();
        let key = dummy_key();
        let devs: Vec<String> = (0..DEVICES).map(|i| format!("dev-{i}")).collect();
        // Warm a realistic working set: one live key per active device.
        for dev in &devs {
            let stamp = match cache.lookup("u", dev, now) {
                Lookup::Miss(s) => s,
                Lookup::Hit(_) => unreachable!(),
            };
            cache.insert("u", dev, Arc::clone(&key), now, stamp);
        }

        println!("device-key cache Mutex<HashMap> contention ({DEVICES} warm entries):");
        for threads in [1usize, 2, 4, 8, 16] {
            let start = Instant::now();
            std::thread::scope(|scope| {
                for t in 0..threads {
                    let cache = cache.clone();
                    let devs = &devs;
                    scope.spawn(move || {
                        // Hot path: a cache HIT — lock, hashmap get, `Arc` clone.
                        for i in 0..OPS_PER_THREAD {
                            let _ = cache.lookup("u", &devs[(t + i) % DEVICES], now);
                        }
                    });
                }
            });
            let elapsed = start.elapsed();
            let total = (threads * OPS_PER_THREAD) as f64;
            let per_op_ns = elapsed.as_nanos() as f64 / total;
            let mops = total / elapsed.as_secs_f64() / 1e6;
            println!(
                "  threads={threads:2}  {per_op_ns:6.1} ns/op  {mops:7.2} Mops/s  ({elapsed:?} for {total:.0} ops)"
            );
        }
    }
}
