//! Device-certificate-signature authentication for write requests.
//!
//! The client signs each write with its device's ML-DSA-44 MLS signing key
//! (`user_device.mls_signature_pub_pq`, bound by its `device_cert`) and the DS
//! verifies against that registered key: no session token, no shared secret at
//! rest. Post-quantum (#668) so an adversary who later breaks Ed25519 cannot
//! forge writes against captured keys.
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
//! The ~3228-character signature header is why no proxy in front of the DS may
//! set a header budget below 8 KiB.
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
//!     For an empty body this is the hex SHA-256 of zero bytes. Binding the
//!     body hash stops a captured signature from being replayed over a
//!     *different* commit.
//!
//! The signature is deterministic ML-DSA-44 over that message; the verifying key
//! is the raw 1312-byte `mls_signature_pub_pq` (openmls `to_public_vec()` for
//! `MLDSA44`), no length prefix.
//!
//! ## Replay window
//!
//! A timestamp more than [`REPLAY_WINDOW_SECS`] (±5 min, as AWS SigV4) from the
//! server clock is rejected. The body hash stops cross-request replay; the window
//! bounds identical-request replay. A nonce store would close it but needs shared
//! write state the DS avoids.
//!
//! ## Never fail open
//!
//! Any error on the auth path is [`AuthRejection::Unauthorized`]; a failed
//! device-key lookup is [`AuthRejection::Unavailable`], a 503 (#1500). The cached
//! entry points take the [`Db`] and check a connection out only on a miss.
//!
//! ## Device-pubkey cache (#658)
//!
//! [`DeviceKeyCache`] supplies only the verifying key, and only after
//! [`parse_credentials`] passes; the signature is still checked in full on every
//! request. Its safety properties: `.codesight/wiki/mls.md` → device-pubkey cache.

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

use crate::db::Db;
use crate::error::AuthRejection;
use crate::last_seen::LastSeenThrottle;

/// Replay window, in seconds, on either side of the server clock.
pub const REPLAY_WINDOW_SECS: i64 = 300;

/// How long a cached device pubkey may be served, measured from insertion and
/// never refreshed on use (a sliding TTL would let an active device keep a stale
/// entry forever). A backstop for a missed eviction hook, not the invalidation
/// mechanism, and well inside the ±300 s replay window already accepted.
pub const DEVICE_KEY_CACHE_TTL_SECS: i64 = 30;

/// Soft cap on live entries. Overflow clears the map, which can only cause a
/// re-read, never an acceptance.
const DEVICE_KEY_CACHE_MAX_ENTRIES: usize = 10_000;

/// One slot per `(user_id, device_id)`. An invalidation drops `key` but bumps and
/// keeps `generation` (a tombstone), so an in-flight read that captured the old
/// generation still loses at insert time (#721). Tombstones expire with the TTL.
struct Slot {
    /// The cached verifying key, or `None` for a bare generation tombstone.
    key: Option<Arc<VerifyingKey>>,
    /// Bumped by every [`DeviceKeyCache::invalidate_device`] of this key.
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

/// Test-only async barrier on the miss path, after the row is read and before
/// the insert: the read-then-evict race window (#721). `None` in production.
type MissBarrier =
    Arc<dyn Fn(&str, &str) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Default)]
struct Inner {
    map: HashMap<(String, String), Slot>,
    /// Bumped by every [`DeviceKeyCache::invalidate_user`] and
    /// [`DeviceKeyCache::clear`]; a miss captures it too. Deliberately coarse:
    /// user-scoped writes are rare, and it closes the race even for a device that
    /// was never individually cached.
    bulk_generation: u64,
    /// See [`MissBarrier`]. Always `None` outside tests.
    miss_barrier: Option<MissBarrier>,
}

/// In-process cache of `(user_id, device_id) -> mls_signature_pub_pq` (#658).
///
/// Correct only because the DS runs as one instance (`max_instances: 1` in
/// `wrangler.{dev,prod}.jsonc`); raising that requires dropping this cache or a
/// shared eviction signal. Negative results are never cached: a missing row is
/// the normal state mid-enrollment, and caching it would let unauthenticated
/// callers grow the map. Every DS write that changes what
/// [`lookup_device_pubkey`] returns must call an `invalidate_*`; the list and the
/// generation-counter race fix (#721) are in `.codesight/wiki/mls.md`.
///
/// Also carries the `last_seen` throttle ([`LastSeenThrottle`]), since it already
/// rides on every cached verification.
#[derive(Clone, Default)]
pub struct DeviceKeyCache {
    inner: Arc<Mutex<Inner>>,
    last_seen: LastSeenThrottle,
}

impl DeviceKeyCache {
    pub fn last_seen(&self) -> &LastSeenThrottle {
        &self.last_seen
    }

    /// Consult the cache. Anything but a live in-TTL key is a [`Lookup::Miss`]
    /// carrying the [`GenStamp`] captured here, under the lock, before the caller
    /// reads the row. A `cached_at` in the future (clock went backwards) is stale.
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

    /// Record a freshly-read live pubkey, unless an invalidation raced the read
    /// (`stamp` is stale): the key may then be the one just revoked.
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
        let now = crate::util::now_unix() as i64;
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

    /// Evict every device of one user, for account-scoped writes. Bumps
    /// `bulk_generation`, so an in-flight read for any of the user's devices loses
    /// its insert (#721).
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

    /// Install the test-only miss barrier (see [`MissBarrier`]).
    pub fn set_miss_barrier(&self, barrier: MissBarrier) {
        self.inner
            .lock()
            .expect("device key cache mutex poisoned")
            .miss_barrier = Some(barrier);
    }
}

const H_USER: &str = "x-pollis-user";
pub(crate) const H_DEVICE: &str = "x-pollis-device";
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
    let body_hash_hex = hex::encode(body_hash);
    format!("{method}\n{path}\n{timestamp}\n{body_hash_hex}").into_bytes()
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

    let sig_bytes =
        crate::util::b64_decode(signature_b64).map_err(|_| AuthRejection::Unauthorized)?;
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
/// [`DeviceLookup::Refused`] covers an absent row, a suspended account (#1213)
/// and a NULL or wrong-length key (the device must re-run `publish-device-cert`,
/// which is never device-signature gated). A DB error is `Err`, never acceptance.
async fn lookup_device_pubkey(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<DeviceLookup> {
    let mut rows = conn
        .query(
            "SELECT mls_signature_pub_pq, revoked_at, \
                    EXISTS (SELECT 1 FROM account_suspension s WHERE s.user_id = ?2) \
             FROM user_device WHERE device_id = ?1 AND user_id = ?2",
            libsql::params![device_id, user_id],
        )
        .await?;

    let row = match rows.next().await? {
        Some(r) => r,
        None => return Ok(DeviceLookup::Refused),
    };

    let pub_bytes: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(0).ok().flatten();
    let key = pub_bytes.and_then(|b| {
        if b.len() != MLDSA44_PUB_LEN {
            return None;
        }
        let encoded: &EncodedVerifyingKey<MlDsa44> = b.as_slice().try_into().ok()?;
        Some(VerifyingKey::decode(encoded))
    });

    // A revoked device must not be able to authenticate, regardless of whether
    // its pubkey column is still populated. Its key is still returned (#1338)
    // so the caller can tell its OWN signature apart — from the same single
    // read, so a revoked device costs exactly what a live one does.
    let revoked_at: Option<String> = row.get::<Option<String>>(1).ok().flatten();
    if revoked_at.is_some() {
        return Ok(match key {
            Some(vk) => DeviceLookup::Revoked(vk),
            None => DeviceLookup::Refused,
        });
    }

    // A suspended account (#1213) authenticates nothing. Read as a required
    // integer so a schema mistake fails closed.
    let suspended: i64 = row.get(2)?;
    if suspended != 0 {
        return Ok(DeviceLookup::Refused);
    }

    Ok(match key {
        Some(vk) => DeviceLookup::Live(vk),
        None => DeviceLookup::Refused,
    })
}

/// What [`lookup_device_pubkey`] found for a `(user_id, device_id)`.
enum DeviceLookup {
    /// A live, enrolled device: its key authenticates the request.
    Live(VerifyingKey),
    /// A REVOKED device that still has a key on record (#1338). Never
    /// authenticates; its key only decides whether the refusal may say
    /// `device_revoked` (the signer's own key) or must stay bare.
    Revoked(VerifyingKey),
    /// Unknown device, suspended account, or no usable key: a bare 401.
    Refused,
}

/// The refusal for a revoked device: [`AuthRejection::DeviceRevoked`] when this
/// request's signature verifies against its own key, the bare
/// [`AuthRejection::Unauthorized`] otherwise. Verifying here costs what a live
/// device's request costs, so the answer's timing tells nothing either.
fn revoked_refusal(
    vk: &VerifyingKey,
    creds: &Credentials,
    method: &str,
    path: &str,
    body: &[u8],
) -> AuthRejection {
    let message = canonical_message(method, path, creds.timestamp, body);
    if vk.verify(&message, &creds.signature).is_ok() {
        AuthRejection::DeviceRevoked
    } else {
        AuthRejection::Unauthorized
    }
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
/// in full; only the Turso read for the verifying key is skipped, and a
/// connection is checked out of `db` only for that read.
pub async fn verify_request_cached(
    cache: &DeviceKeyCache,
    db: &Db,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<String, AuthRejection> {
    Ok(
        verify_request_identity_cached(cache, db, headers, method, path, body, now)
            .await?
            .0,
    )
}

/// [`verify_request`] returning `(user_id, device_id)`, exactly the values the
/// signature was checked against. Use it to key anything on the device (#681),
/// never an unauthenticated body field.
pub async fn verify_request_identity(
    conn: &Connection,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<(String, String), AuthRejection> {
    verify_request_inner(None, KeySource::Conn(conn), headers, method, path, body, now).await
}

/// [`verify_request_identity`], served from `cache` on a hit (#658). Same
/// contract, same rejections — only the pubkey READ is elided, never the
/// signature check.
pub async fn verify_request_identity_cached(
    cache: &DeviceKeyCache,
    db: &Db,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
    now: i64,
) -> Result<(String, String), AuthRejection> {
    verify_request_inner(Some(cache), KeySource::Db(db), headers, method, path, body, now).await
}

/// Where a device key is read from when it has to be read at all.
#[derive(Clone, Copy)]
enum KeySource<'a> {
    /// A connection the caller already holds (the uncached entry points).
    Conn(&'a Connection),
    /// The pool, checked out only for the read (#1500): a request served from
    /// the cache never takes a connection.
    Db(&'a Db),
}

/// [`lookup_device_pubkey`] against `source`. A database failure is
/// [`AuthRejection::Unavailable`] — a refusal, so never an acceptance, but a 503
/// rather than a 401 (#1500).
async fn read_device(
    source: KeySource<'_>,
    user_id: &str,
    device_id: &str,
) -> Result<DeviceLookup, AuthRejection> {
    let unavailable = |e: anyhow::Error| {
        crate::error::note_transient(&e);
        tracing::error!("device key lookup failed: {e:#}");
        AuthRejection::Unavailable
    };
    match source {
        KeySource::Conn(conn) => lookup_device_pubkey(conn, user_id, device_id)
            .await
            .map_err(unavailable),
        KeySource::Db(db) => {
            let conn = db.conn().await.map_err(unavailable)?;
            lookup_device_pubkey(&conn, user_id, device_id)
                .await
                .map_err(unavailable)
        }
    }
}

/// The one implementation of the verify path. `cache: None` is the uncached path
/// the in-process integration harnesses use.
async fn verify_request_inner(
    cache: Option<&DeviceKeyCache>,
    source: KeySource<'_>,
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
        Some(c) => match c.lookup(&creds.user_id, &creds.device_id, now) {
            Lookup::Hit(vk) => vk,
            Lookup::Miss(stamp) => {
                // Never fail open, and never cache the negative.
                let vk = match read_device(source, &creds.user_id, &creds.device_id).await? {
                    DeviceLookup::Live(vk) => Arc::new(vk),
                    DeviceLookup::Revoked(vk) => {
                        return Err(revoked_refusal(&vk, &creds, method, path, body));
                    }
                    DeviceLookup::Refused => return Err(AuthRejection::Unauthorized),
                };
                // Test seam (#721): park between the read and the insert.
                if let Some(barrier) = c.miss_barrier() {
                    barrier(&creds.user_id, &creds.device_id).await;
                }
                c.insert(&creds.user_id, &creds.device_id, Arc::clone(&vk), now, stamp);
                vk
            }
        },
        // Uncached path (the test harnesses).
        None => match read_device(source, &creds.user_id, &creds.device_id).await? {
            DeviceLookup::Live(vk) => Arc::new(vk),
            DeviceLookup::Revoked(vk) => {
                return Err(revoked_refusal(&vk, &creds, method, path, body));
            }
            DeviceLookup::Refused => return Err(AuthRejection::Unauthorized),
        },
    };

    let message = canonical_message(method, path, creds.timestamp, body);
    verifying_key
        .verify(&message, &creds.signature)
        .map_err(|_| AuthRejection::Unauthorized)?;

    // The device just proved it is alive: bump `last_seen`, throttled and spawned
    // so a failure never fails this request. Only the shipping (cached) path.
    if let (Some(c), KeySource::Db(db)) = (cache, source) {
        c.last_seen().note(db, &creds.user_id, &creds.device_id, now);
    }

    Ok((creds.user_id, creds.device_id))
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

    /// Lock-contention microbenchmark for the cache's `Mutex<HashMap>` (#721).
    ///
    /// Run: `cargo test -p pollis-delivery --lib bench::mutex_contention -- --ignored --nocapture`
    #[test]
    #[ignore = "microbenchmark; run with --ignored --nocapture"]
    fn mutex_contention() {
        const DEVICES: usize = 256;
        const OPS_PER_THREAD: usize = 200_000;

        let cache = DeviceKeyCache::default();
        let now = crate::util::now_unix() as i64;
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
