//! Per-target routing policy: overlay vs direct vs degraded-error, plus **live
//! relay revocation**.
//!
//! This encodes the plane split (design §6.4) and the first-party allowlist as
//! *data*, and it is pure and unit-testable — no sockets. The shim asks
//! [`RoutingPolicy::plan`] what to do with a target, then executes; if an overlay
//! attempt fails, [`PlannedRoute::fallback_to_direct`] decides whether to fall
//! back to a direct dial (Prefer) or surface a degraded error (Strict).
//!
//! The second half of this module ([`RevocationStore`] and friends) is the
//! *identity* side of the same question: a relay may be in the signed directory
//! and still be untrustworthy, because the directory is only re-signed with a
//! ~1 hour TTL and a seized node stays "valid" until that lapses. Revocation is a
//! second signed artifact with a much shorter life; see [`verify_revocations`].
//! It is pure too — bytes in, decision out — so both the client (path selection)
//! and the relay (refusing to extend a circuit to a revoked next hop) can share
//! one enforcement point with no I/O of its own.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::server::{Allowlist, HostPattern};

/// The user's overlay mode (design §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    /// Overlay inert: every target routes direct.
    Off,
    /// Control-plane hosts route overlay, falling back to direct on failure.
    Prefer,
    /// Control-plane hosts MUST route overlay; on failure, degrade (never drop,
    /// never silently go direct) — messages-must-work.
    Strict,
}

impl OverlayMode {
    /// Stable `u8` encoding so the mode can live in an [`AtomicU8`] the shim's
    /// routing policy reads live (runtime Prefer↔Strict, design §14 / apply slice).
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            OverlayMode::Off => 0,
            OverlayMode::Prefer => 1,
            OverlayMode::Strict => 2,
        }
    }

    pub(crate) fn from_u8(v: u8) -> OverlayMode {
        match v {
            1 => OverlayMode::Prefer,
            2 => OverlayMode::Strict,
            _ => OverlayMode::Off,
        }
    }
}

/// What the shim should attempt for a given target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedRoute {
    /// Dial the target directly.
    Direct,
    /// Route through the overlay. If the circuit can't be built, fall back to a
    /// direct dial when `fallback_to_direct`, else surface a degraded error.
    Overlay { fallback_to_direct: bool },
}

/// The final action, after an overlay attempt is known to have succeeded or
/// failed. Pure — used both by the shim and by unit tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalAction {
    Direct,
    Overlay,
    /// Strict mode with no usable circuit: surface an error to the caller.
    Degraded,
}

/// Decides how each target is routed, given the mode, the set of overlay
/// (control-plane) hosts, and the set of always-direct (media) hosts.
///
/// `mode` is held in a shared [`AtomicU8`] rather than a plain field so a running
/// shim can be flipped between `Prefer` and `Strict` **live** — no shim restart,
/// no DB reconnect — via [`OverlayHandle::set_mode`](crate::shim::OverlayHandle::set_mode).
/// The host sets are immutable for the shim's lifetime (a mode change never
/// re-derives the plane split).
#[derive(Clone)]
pub struct RoutingPolicy {
    mode: Arc<AtomicU8>,
    /// Hosts routed through the overlay when the mode allows (control plane).
    overlay_hosts: Vec<HostPattern>,
    /// Hosts always dialed directly regardless of mode (media plane, §6.4).
    direct_hosts: Vec<HostPattern>,
}

impl RoutingPolicy {
    pub fn new(mode: OverlayMode, overlay_hosts: Allowlist, direct_hosts: Allowlist) -> RoutingPolicy {
        RoutingPolicy {
            mode: Arc::new(AtomicU8::new(mode.as_u8())),
            overlay_hosts: overlay_hosts.into_patterns(),
            direct_hosts: direct_hosts.into_patterns(),
        }
    }

    pub fn mode(&self) -> OverlayMode {
        OverlayMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    /// Flip the live routing mode. The shim's next `plan()` observes it — used to
    /// switch Prefer↔Strict without restarting the shim (design §14 apply slice).
    pub fn set_mode(&self, mode: OverlayMode) {
        self.mode.store(mode.as_u8(), Ordering::Relaxed);
    }

    /// A handle onto the shared mode cell, so the [`OverlayHandle`] can flip it
    /// after the policy has been moved into the shim.
    pub(crate) fn mode_atomic(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.mode)
    }

    fn is_direct_host(&self, host: &str) -> bool {
        self.direct_hosts.iter().any(|p| p.matches(host))
    }

    fn is_overlay_host(&self, host: &str) -> bool {
        self.overlay_hosts.iter().any(|p| p.matches(host))
    }

    /// Decide the intended route for `host`.
    ///
    /// - Media/direct hosts → Direct in every mode.
    /// - `Off` → Direct for everything.
    /// - Control-plane host + `Prefer` → Overlay (direct fallback).
    /// - Control-plane host + `Strict` → Overlay (degrade on failure).
    /// - Any other host → Direct (e.g. non-first-party like Expo push, §14.4).
    pub fn plan(&self, host: &str) -> PlannedRoute {
        // The media plane is always direct — it must never be taxed by the
        // overlay, even in Strict mode (§6.4).
        if self.is_direct_host(host) {
            return PlannedRoute::Direct;
        }
        match self.mode() {
            OverlayMode::Off => PlannedRoute::Direct,
            OverlayMode::Prefer if self.is_overlay_host(host) => {
                PlannedRoute::Overlay { fallback_to_direct: true }
            }
            OverlayMode::Strict if self.is_overlay_host(host) => {
                PlannedRoute::Overlay { fallback_to_direct: false }
            }
            // Not a control-plane host: nothing to hide here, dial direct.
            _ => PlannedRoute::Direct,
        }
    }

    #[cfg(test)]
    pub(crate) fn mode_handle_for_test(&self) -> Arc<AtomicU8> {
        self.mode_atomic()
    }

    /// Pure resolution of a plan given whether an overlay attempt succeeded.
    /// `overlay_ok == None` means no attempt was made (a Direct plan).
    pub fn reconcile(plan: PlannedRoute, overlay_ok: Option<bool>) -> FinalAction {
        match plan {
            PlannedRoute::Direct => FinalAction::Direct,
            PlannedRoute::Overlay { fallback_to_direct } => match overlay_ok {
                Some(true) => FinalAction::Overlay,
                Some(false) if fallback_to_direct => FinalAction::Direct,
                Some(false) => FinalAction::Degraded,
                None => FinalAction::Degraded,
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Live relay revocation (#813 Phase C)
// ─────────────────────────────────────────────────────────────────────────────

/// The `type` discriminator every revocation payload must carry.
///
/// The revocation list and the signed directory are produced by the **same**
/// Ed25519 key, so without a discriminator an attacker could feed a directory
/// envelope to the revocation parser (or the reverse) and have the signature
/// check pass. Both artifacts name themselves; a mismatch is a hard reject.
pub const REVOCATION_TYPE: &str = "pollis-relay-revocations";

/// The only revocation payload version this build understands.
pub const REVOCATION_VERSION: u32 = 1;

/// Why a revocation list was rejected. **Every variant is fail-closed**: a
/// rejected list never becomes evidence that a relay is fine, it removes the
/// evidence entirely, and [`RevocationStore::admit`] then answers
/// [`Admission::Unevaluable`] — which is not an admission.
#[derive(Debug, thiserror::Error)]
pub enum RevocationError {
    #[error("malformed revocation envelope JSON")]
    MalformedEnvelope,
    #[error("malformed revocation payload JSON")]
    MalformedPayload,
    #[error("bad base64 in {0}")]
    BadBase64(&'static str),
    #[error("pinned directory key is not a 32-byte Ed25519 public key")]
    BadPinnedKey,
    #[error("signature is not 64 bytes")]
    BadSignatureLen,
    #[error("bad signature")]
    BadSignature,
    #[error("unsupported revocation version {0}")]
    UnsupportedVersion(u32),
    #[error("wrong artifact type {0:?} (expected {REVOCATION_TYPE:?})")]
    WrongType(String),
    #[error("revocation list expired (now {now} >= expires_at {expires_at})")]
    Expired { now: i64, expires_at: i64 },
    #[error("revocation list rolled back (seq {seq} < required {floor})")]
    RolledBack { seq: u64, floor: u64 },
    #[error("revocation entry is not enforceable by this build: {0}")]
    Unenforceable(&'static str),
    /// Revocation is enforced but no key was configured to verify it against.
    #[error("no pinned directory key configured — revocation cannot be evaluated")]
    NotConfigured,
}

/// The signed envelope, byte-identical in shape to the directory's:
/// base64 (STANDARD) of the payload bytes plus base64 of the 64-byte Ed25519
/// signature over **exactly** those bytes. No canonicalization on either side.
#[derive(Deserialize)]
struct RevocationEnvelope {
    payload_b64: String,
    signature_b64: String,
}

/// The revocation payload — the exact bytes that get base64'd into `payload_b64`.
#[derive(Deserialize)]
struct RevocationPayload {
    version: u32,
    #[serde(rename = "type")]
    kind: String,
    seq: u64,
    issued_at: i64,
    expires_at: i64,
    revoked: Vec<RevokedEntry>,
}

/// One revocation. Both selectors are optional in the wire format but an entry
/// carrying **neither** is rejected (see [`RevocationError::Unenforceable`]).
#[derive(Deserialize)]
struct RevokedEntry {
    #[serde(default)]
    addr: Option<String>,
    #[serde(default)]
    cert_sha256_b64: Option<String>,
}

/// A verified, unexpired, non-rolled-back revocation list.
///
/// Construct one only via [`verify_revocations`] — there is deliberately no way
/// to build this type from unverified data, so "I have a `RevocationList`" is
/// itself the proof that the signature, version, type, expiry and sequence
/// checks all passed.
#[derive(Debug, Clone)]
pub struct RevocationList {
    seq: u64,
    issued_at: i64,
    expires_at: i64,
    /// Revoked endpoints, lowercased `host:port`.
    addrs: HashSet<String>,
    /// Revoked identities: base64 (STANDARD) of SHA-256 over the DER leaf.
    cert_digests: HashSet<String>,
}

impl RevocationList {
    /// The publisher's monotonic sequence number (the SSM parameter version of
    /// the operator-authored revocation set — see `infra/relay-hydra`).
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    /// How many relays this list revokes. An **empty list is legal and normal** —
    /// unlike the directory's `relays`, where empty is a reject. "Nothing is
    /// revoked right now" is the steady state and must be expressible, otherwise
    /// there would be no fresh artifact to fail closed against.
    pub fn len(&self) -> usize {
        self.addrs.len() + self.cert_digests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Is `now_secs` still inside this list's validity window?
    pub fn is_fresh(&self, now_secs: i64) -> bool {
        now_secs < self.expires_at
    }

    /// Does this list revoke `id`? Either selector matching is enough: the
    /// address covers the shared-pool-identity case (every hydra node currently
    /// presents the SAME pinned leaf, so a cert-keyed revocation would take the
    /// whole pool down), the cert digest covers per-node and peer-hosted
    /// identities where the leaf really is the relay's name (§7).
    pub fn revokes(&self, id: &RelayIdentity<'_>) -> bool {
        if self.addrs.contains(&normalize_addr(id.addr)) {
            return true;
        }
        self.cert_digests.contains(&cert_digest_b64(id.cert_der))
    }
}

/// The relay being checked: the endpoint the directory advertised plus the DER
/// leaf it actually presented on the wire.
///
/// Both are carried on purpose. The address is what an operator can revoke the
/// instant a node is seized; the cert is what remains true if that node moves.
pub struct RelayIdentity<'a> {
    pub addr: &'a str,
    pub cert_der: &'a [u8],
}

/// The outcome of a revocation check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Fresh evidence exists and it does not revoke this relay.
    Admit,
    /// Fresh evidence positively revokes this relay.
    Revoked,
    /// **No usable evidence**: never loaded, expired, or revocation was never
    /// configured on this node. Not an admission — see [`Admission::admitted`].
    Unevaluable,
}

impl Admission {
    /// The single place the fail-closed rule is encoded: only [`Admission::Admit`]
    /// is an admission. A caller that cannot evaluate revocation must not use the
    /// relay, so "I don't know" and "revoked" are the same answer here.
    pub fn admitted(self) -> bool {
        matches!(self, Admission::Admit)
    }
}

/// Verify a revocation envelope against the pinned directory-signing key.
///
/// Mirrors `pollis-core`'s directory verifier and the JS reference
/// (`infra/relay-hydra/lib/revocation-verify.mjs`) exactly: verify the Ed25519
/// signature over the decoded payload bytes FIRST, then parse and range-check.
///
/// `seq_floor` is the lowest sequence number the caller will accept. Pass the
/// highest sequence it has already seen (rollback protection) or the sequence the
/// signed directory anchored (freshness binding) — whichever is greater. A list
/// below the floor is a **replay** and is rejected.
pub fn verify_revocations(
    envelope_bytes: &[u8],
    pinned_pubkey_b64: &str,
    now_secs: i64,
    seq_floor: u64,
) -> Result<RevocationList, RevocationError> {
    let envelope: RevocationEnvelope = serde_json::from_slice(envelope_bytes)
        .map_err(|_| RevocationError::MalformedEnvelope)?;

    let payload = B64
        .decode(envelope.payload_b64.as_bytes())
        .map_err(|_| RevocationError::BadBase64("payload_b64"))?;
    let signature_bytes = B64
        .decode(envelope.signature_b64.as_bytes())
        .map_err(|_| RevocationError::BadBase64("signature_b64"))?;

    let verifying_key = verifying_key_from_b64(pinned_pubkey_b64)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| RevocationError::BadSignatureLen)?;
    verifying_key
        .verify_strict(&payload, &signature)
        .map_err(|_| RevocationError::BadSignature)?;

    let payload: RevocationPayload =
        serde_json::from_slice(&payload).map_err(|_| RevocationError::MalformedPayload)?;

    if payload.version != REVOCATION_VERSION {
        return Err(RevocationError::UnsupportedVersion(payload.version));
    }
    // Cross-artifact confusion guard — see REVOCATION_TYPE.
    if payload.kind != REVOCATION_TYPE {
        return Err(RevocationError::WrongType(payload.kind));
    }
    if now_secs >= payload.expires_at {
        return Err(RevocationError::Expired {
            now: now_secs,
            expires_at: payload.expires_at,
        });
    }
    if payload.seq < seq_floor {
        return Err(RevocationError::RolledBack {
            seq: payload.seq,
            floor: seq_floor,
        });
    }

    let mut addrs = HashSet::new();
    let mut cert_digests = HashSet::new();
    for entry in payload.revoked {
        // An entry this build cannot act on fails the WHOLE list closed rather
        // than being skipped. Skipping would silently downgrade a revocation
        // published by a newer producer (a selector we don't know yet) into an
        // admission — exactly the bypass revocation exists to prevent.
        let addr = entry.addr.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let digest = entry
            .cert_sha256_b64
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if addr.is_none() && digest.is_none() {
            return Err(RevocationError::Unenforceable(
                "entry carries no addr and no cert_sha256_b64",
            ));
        }
        if let Some(addr) = addr {
            addrs.insert(normalize_addr(addr));
        }
        if let Some(digest) = digest {
            // A digest that is not 32 bytes can never match a real SHA-256, so
            // it is an unenforceable revocation, not a harmless one.
            let raw = B64
                .decode(digest.as_bytes())
                .map_err(|_| RevocationError::Unenforceable("cert_sha256_b64 is not base64"))?;
            if raw.len() != 32 {
                return Err(RevocationError::Unenforceable(
                    "cert_sha256_b64 is not a 32-byte SHA-256",
                ));
            }
            cert_digests.insert(B64.encode(raw));
        }
    }

    Ok(RevocationList {
        seq: payload.seq,
        issued_at: payload.issued_at,
        expires_at: payload.expires_at,
        addrs,
        cert_digests,
    })
}

/// Rebuild an Ed25519 [`VerifyingKey`] from the base64 of its raw 32 bytes (the
/// `POLLIS_OVERLAY_DIRECTORY_KEY` shape).
fn verifying_key_from_b64(pinned_pubkey_b64: &str) -> Result<VerifyingKey, RevocationError> {
    let raw = B64
        .decode(pinned_pubkey_b64.trim().as_bytes())
        .map_err(|_| RevocationError::BadBase64("pinned key"))?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| RevocationError::BadPinnedKey)?;
    VerifyingKey::from_bytes(&raw).map_err(|_| RevocationError::BadPinnedKey)
}

fn normalize_addr(addr: &str) -> String {
    addr.trim().to_ascii_lowercase()
}

fn cert_digest_b64(cert_der: &[u8]) -> String {
    B64.encode(Sha256::digest(cert_der))
}

/// The live revocation state a relay (or a client's path selector) consults.
///
/// Cheap to clone — every clone shares one cell, so a refresh task installing a
/// newer list is observed immediately by every in-flight `Extend`.
///
/// **Fail-closed by construction:**
/// - a store with no pinned key ([`RevocationStore::unconfigured`]) admits
///   nothing;
/// - a store that has never installed a list admits nothing;
/// - a store whose list has expired admits nothing;
/// - [`RevocationStore::install`] never accepts a list below the highest
///   sequence already seen, so a captured older-but-unexpired list cannot be
///   replayed to un-revoke a relay.
#[derive(Clone)]
pub struct RevocationStore {
    inner: Arc<RwLock<StoreInner>>,
    /// `None` ⇒ this node cannot evaluate revocation at all.
    pinned_key_b64: Option<Arc<String>>,
}

struct StoreInner {
    list: Option<RevocationList>,
    /// The highest sequence ever accepted. Monotonic; survives a rejected
    /// install, so a rollback attempt cannot lower the bar for the next one.
    high_water: u64,
}

impl RevocationStore {
    /// An empty store. Whether it can evaluate anything is decided entirely by
    /// whether a directory key came with it.
    fn with_key(pinned_key_b64: Option<Arc<String>>) -> RevocationStore {
        RevocationStore {
            inner: Arc::new(RwLock::new(StoreInner {
                list: None,
                high_water: 0,
            })),
            pinned_key_b64,
        }
    }

    /// A store that verifies against `pinned_pubkey_b64` (the same
    /// `POLLIS_OVERLAY_DIRECTORY_KEY` that pins the directory).
    pub fn enforcing(pinned_pubkey_b64: impl Into<String>) -> RevocationStore {
        RevocationStore::with_key(Some(Arc::new(pinned_pubkey_b64.into())))
    }

    /// A store for a node that was never given a directory key.
    ///
    /// This is **not** a bypass: it admits nothing. A relay deployed without the
    /// key simply cannot extend circuits to other relays (single-hop
    /// `CONNECT` is unaffected — there is no next hop to vouch for). Making the
    /// unconfigured case permissive is the classic way revocation turns into
    /// decoration, so it is deliberately the strictest state, not the loosest.
    pub fn unconfigured() -> RevocationStore {
        RevocationStore::with_key(None)
    }

    /// Is this store able to evaluate revocation at all?
    pub fn is_configured(&self) -> bool {
        self.pinned_key_b64.is_some()
    }

    /// Verify `envelope_bytes` and adopt it if it is newer-or-equal to what we
    /// hold. Returns the accepted sequence number.
    ///
    /// `directory_seq_floor` is the sequence the signed directory anchored, if
    /// the caller has one; the effective floor is the max of that and the
    /// store's own high-water mark. Binding to the directory is what stops an
    /// on-path attacker pairing a *fresh* directory with a *stale but unexpired*
    /// revocation list to hide the newest revocation.
    pub fn install(
        &self,
        envelope_bytes: &[u8],
        now_secs: i64,
        directory_seq_floor: u64,
    ) -> Result<u64, RevocationError> {
        let key = self
            .pinned_key_b64
            .as_ref()
            .ok_or(RevocationError::NotConfigured)?;
        let floor = {
            let guard = self.read();
            guard.high_water.max(directory_seq_floor)
        };
        let list = verify_revocations(envelope_bytes, key, now_secs, floor)?;
        let seq = list.seq;
        let mut guard = self.write();
        // Re-check under the write lock: two concurrent installs must not let the
        // older one win.
        if seq < guard.high_water {
            return Err(RevocationError::RolledBack {
                seq,
                floor: guard.high_water,
            });
        }
        guard.high_water = seq;
        guard.list = Some(list);
        Ok(seq)
    }

    /// The sequence of the currently-held list, if any.
    pub fn seq(&self) -> Option<u64> {
        self.read().list.as_ref().map(RevocationList::seq)
    }

    /// The highest sequence ever accepted (never decreases).
    pub fn high_water(&self) -> u64 {
        self.read().high_water
    }

    /// Decide whether `id` may be used right now.
    pub fn admit(&self, id: &RelayIdentity<'_>, now_secs: i64) -> Admission {
        if self.pinned_key_b64.is_none() {
            return Admission::Unevaluable;
        }
        let guard = self.read();
        let Some(list) = guard.list.as_ref() else {
            return Admission::Unevaluable;
        };
        // Staleness is checked at USE time, not just at install time: a list
        // installed 10 minutes ago is worthless evidence now, and treating it as
        // "not revoked" would restore exactly the TTL-window bug revocation
        // exists to close.
        if !list.is_fresh(now_secs) {
            return Admission::Unevaluable;
        }
        if list.revokes(id) {
            return Admission::Revoked;
        }
        Admission::Admit
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, StoreInner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, StoreInner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for RevocationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.read();
        f.debug_struct("RevocationStore")
            .field("configured", &self.pinned_key_b64.is_some())
            .field("seq", &guard.list.as_ref().map(RevocationList::seq))
            .field("high_water", &guard.high_water)
            .field("revoked", &guard.list.as_ref().map(RevocationList::len))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_mode_flips_plan_live() {
        let policy = RoutingPolicy::new(
            OverlayMode::Prefer,
            Allowlist::from_patterns(["control.test"]),
            Allowlist::default(),
        );
        // Prefer: control-plane host routes overlay with direct fallback.
        assert_eq!(
            policy.plan("control.test"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        // Flip to Strict live — same policy object, no rebuild.
        policy.set_mode(OverlayMode::Strict);
        assert_eq!(
            policy.plan("control.test"),
            PlannedRoute::Overlay { fallback_to_direct: false }
        );
        // A clone shares the same cell — this is what the OverlayHandle holds.
        let handle_cell = policy.mode_handle_for_test();
        handle_cell.store(OverlayMode::Off.as_u8(), Ordering::Relaxed);
        assert_eq!(policy.mode(), OverlayMode::Off);
        assert_eq!(policy.plan("control.test"), PlannedRoute::Direct);
    }

    // ── Live relay revocation (#813 Phase C) ────────────────────────────────

    mod revocation {
        use super::*;
        use ed25519_dalek::{Signer, SigningKey};

        const NOW: i64 = 1_737_600_000;
        const CERT: &[u8] = b"a relay leaf, DER in real life";
        const OTHER_CERT: &[u8] = b"a different relay leaf";

        // Deterministic keys so nothing here touches the RNG.
        fn signer() -> SigningKey {
            SigningKey::from_bytes(&[3u8; 32])
        }

        fn attacker() -> SigningKey {
            SigningKey::from_bytes(&[4u8; 32])
        }

        fn pinned(sk: &SigningKey) -> String {
            B64.encode(sk.verifying_key().to_bytes())
        }

        /// Sign the EXACT payload bytes, exactly as the reconciler does.
        fn envelope(sk: &SigningKey, payload_json: &str) -> Vec<u8> {
            let payload = payload_json.as_bytes();
            let sig = sk.sign(payload);
            let env = serde_json::json!({
                "payload_b64": B64.encode(payload),
                "signature_b64": B64.encode(sig.to_bytes()),
            });
            serde_json::to_vec(&env).unwrap()
        }

        fn digest_of(cert: &[u8]) -> String {
            cert_digest_b64(cert)
        }

        /// A well-formed payload revoking exactly `revoked` (already JSON).
        fn payload(seq: u64, expires_at: i64, revoked: &str) -> String {
            format!(
                r#"{{"version":1,"type":"{REVOCATION_TYPE}","seq":{seq},"issued_at":{NOW},"expires_at":{expires_at},"revoked":[{revoked}]}}"#
            )
        }

        fn id<'a>(addr: &'a str, cert: &'a [u8]) -> RelayIdentity<'a> {
            RelayIdentity {
                addr,
                cert_der: cert,
            }
        }

        #[test]
        fn empty_list_is_valid_and_admits() {
            let sk = signer();
            let env = envelope(&sk, &payload(1, NOW + 300, ""));
            let store = RevocationStore::enforcing(pinned(&sk));
            assert_eq!(store.install(&env, NOW, 0).unwrap(), 1);
            assert!(store.seq().is_some());
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Admit
            );
        }

        #[test]
        fn revoked_relay_is_rejected_by_address() {
            let sk = signer();
            let env = envelope(
                &sk,
                &payload(2, NOW + 300, r#"{"addr":"203.0.113.7:9444","reason":"seized"}"#),
            );
            let store = RevocationStore::enforcing(pinned(&sk));
            store.install(&env, NOW, 0).unwrap();

            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Revoked
            );
            // Case/whitespace normalized on both sides.
            assert_eq!(
                store.admit(&id(" 203.0.113.7:9444 ", CERT), NOW),
                Admission::Revoked
            );
            // A different node in the same pool — sharing the SAME pinned leaf —
            // is untouched. This is why address is a first-class selector.
            assert_eq!(
                store.admit(&id("203.0.113.8:9444", CERT), NOW),
                Admission::Admit
            );
        }

        #[test]
        fn revoked_relay_is_rejected_by_cert_digest() {
            let sk = signer();
            let entry = format!(r#"{{"cert_sha256_b64":"{}"}}"#, digest_of(CERT));
            let env = envelope(&sk, &payload(2, NOW + 300, &entry));
            let store = RevocationStore::enforcing(pinned(&sk));
            store.install(&env, NOW, 0).unwrap();

            // Identity-keyed: the relay is refused wherever it moves to.
            assert_eq!(
                store.admit(&id("198.51.100.4:9444", CERT), NOW),
                Admission::Revoked
            );
            assert_eq!(
                store.admit(&id("198.51.100.4:9444", OTHER_CERT), NOW),
                Admission::Admit
            );
        }

        #[test]
        fn forged_revocation_is_rejected() {
            let sk = signer();
            // Signed by someone else entirely.
            let forged = envelope(&attacker(), &payload(9, NOW + 300, ""));
            let store = RevocationStore::enforcing(pinned(&sk));
            assert!(matches!(
                store.install(&forged, NOW, 0),
                Err(RevocationError::BadSignature)
            ));
            // ...and the store learned nothing from it.
            assert!(store.seq().is_none());
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Unevaluable
            );
        }

        #[test]
        fn tampered_payload_is_rejected() {
            let sk = signer();
            let good = envelope(
                &sk,
                &payload(3, NOW + 300, r#"{"addr":"203.0.113.7:9444"}"#),
            );
            let mut env: serde_json::Value = serde_json::from_slice(&good).unwrap();
            // Swap in a payload that revokes nothing, keeping the old signature —
            // i.e. an attacker trying to STRIP a revocation.
            let stripped = payload(3, NOW + 300, "");
            env["payload_b64"] = serde_json::Value::String(
                B64.encode(stripped.as_bytes()),
            );
            let bytes = serde_json::to_vec(&env).unwrap();
            let store = RevocationStore::enforcing(pinned(&sk));
            assert!(matches!(
                store.install(&bytes, NOW, 0),
                Err(RevocationError::BadSignature)
            ));
        }

        #[test]
        fn a_directory_envelope_cannot_pose_as_a_revocation_list() {
            let sk = signer();
            // A genuinely signed DIRECTORY, replayed at the revocation endpoint.
            // Same key, same envelope shape — only the type tag stops it.
            let directory = format!(
                r#"{{"version":1,"issued_at":{NOW},"expires_at":{},"relays":[{{"addr":"203.0.113.7:9444","region":"us-west-2","cert_b64":"QUJD"}}]}}"#,
                NOW + 3600
            );
            let env = envelope(&sk, &directory);
            let store = RevocationStore::enforcing(pinned(&sk));
            assert!(matches!(
                store.install(&env, NOW, 0),
                Err(RevocationError::MalformedPayload | RevocationError::WrongType(_))
            ));
        }

        #[test]
        fn expired_list_stops_admitting_without_being_reinstalled() {
            let sk = signer();
            let env = envelope(&sk, &payload(1, NOW + 300, ""));
            let store = RevocationStore::enforcing(pinned(&sk));
            store.install(&env, NOW, 0).unwrap();
            assert_eq!(store.admit(&id("203.0.113.7:9444", CERT), NOW), Admission::Admit);

            // One second past expiry the SAME held list is no longer evidence.
            // This is the property that bounds the exposure window to the
            // revocation TTL rather than the directory TTL.
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW + 300),
                Admission::Unevaluable
            );
            // And it cannot be re-installed either.
            assert!(matches!(
                store.install(&env, NOW + 300, 0),
                Err(RevocationError::Expired { .. })
            ));
        }

        #[test]
        fn replayed_older_list_cannot_un_revoke() {
            let sk = signer();
            let old = envelope(&sk, &payload(5, NOW + 300, ""));
            let new = envelope(
                &sk,
                &payload(6, NOW + 300, r#"{"addr":"203.0.113.7:9444"}"#),
            );
            let store = RevocationStore::enforcing(pinned(&sk));
            store.install(&new, NOW, 0).unwrap();
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Revoked
            );

            // Replaying the earlier, still-unexpired, genuinely-signed list must
            // not restore the relay.
            assert!(matches!(
                store.install(&old, NOW, 0),
                Err(RevocationError::RolledBack { seq: 5, floor: 6 })
            ));
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Revoked
            );
            assert_eq!(store.high_water(), 6);
        }

        #[test]
        fn directory_anchor_rejects_a_stale_but_unexpired_list() {
            let sk = signer();
            // A fresh directory says "revocation seq is at least 9". Pairing it
            // with a valid seq-7 list is the on-path downgrade attack.
            let stale = envelope(&sk, &payload(7, NOW + 300, ""));
            let store = RevocationStore::enforcing(pinned(&sk));
            assert!(matches!(
                store.install(&stale, NOW, 9),
                Err(RevocationError::RolledBack { seq: 7, floor: 9 })
            ));
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Unevaluable
            );
            // The matching list is accepted.
            let current = envelope(&sk, &payload(9, NOW + 300, ""));
            assert_eq!(store.install(&current, NOW, 9).unwrap(), 9);
        }

        #[test]
        fn unconfigured_store_admits_nothing() {
            let store = RevocationStore::unconfigured();
            assert!(!store.is_configured());
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Unevaluable
            );
            assert!(matches!(
                store.install(b"{}", NOW, 0),
                Err(RevocationError::NotConfigured)
            ));
        }

        #[test]
        fn never_loaded_store_admits_nothing() {
            let store = RevocationStore::enforcing(pinned(&signer()));
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Unevaluable
            );
            assert!(!Admission::Unevaluable.admitted());
            assert!(!Admission::Revoked.admitted());
            assert!(Admission::Admit.admitted());
        }

        #[test]
        fn entry_with_no_usable_selector_fails_the_whole_list_closed() {
            let sk = signer();
            // A revocation published by a NEWER producer, keyed on something this
            // build does not understand. Silently skipping it would admit a relay
            // the operator revoked.
            let env = envelope(
                &sk,
                &payload(4, NOW + 300, r#"{"region":"us-west-2","reason":"seized"}"#),
            );
            let store = RevocationStore::enforcing(pinned(&sk));
            assert!(matches!(
                store.install(&env, NOW, 0),
                Err(RevocationError::Unenforceable(_))
            ));
            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Unevaluable
            );
        }

        #[test]
        fn malformed_cert_digest_fails_the_whole_list_closed() {
            let sk = signer();
            for bad in [r#"{"cert_sha256_b64":"!!!not base64!!!"}"#, r#"{"cert_sha256_b64":"QUJD"}"#] {
                let env = envelope(&sk, &payload(4, NOW + 300, bad));
                let store = RevocationStore::enforcing(pinned(&sk));
                assert!(
                    matches!(store.install(&env, NOW, 0), Err(RevocationError::Unenforceable(_))),
                    "expected {bad} to be unenforceable"
                );
            }
        }

        #[test]
        fn rejects_wrong_version_and_malformed_envelopes() {
            let sk = signer();
            let key = pinned(&sk);
            let v2 = format!(
                r#"{{"version":2,"type":"{REVOCATION_TYPE}","seq":1,"issued_at":{NOW},"expires_at":{},"revoked":[]}}"#,
                NOW + 300
            );
            assert!(matches!(
                verify_revocations(&envelope(&sk, &v2), &key, NOW, 0),
                Err(RevocationError::UnsupportedVersion(2))
            ));
            assert!(matches!(
                verify_revocations(b"not json at all", &key, NOW, 0),
                Err(RevocationError::MalformedEnvelope)
            ));
            // A pinned key that is not 32 bytes can never verify anything.
            assert!(matches!(
                verify_revocations(&envelope(&sk, &payload(1, NOW + 300, "")), "QUJD", NOW, 0),
                Err(RevocationError::BadPinnedKey)
            ));
        }

        #[test]
        fn store_clones_share_one_cell() {
            let sk = signer();
            let store = RevocationStore::enforcing(pinned(&sk));
            let clone = store.clone();
            let env = envelope(
                &sk,
                &payload(11, NOW + 300, r#"{"addr":"203.0.113.7:9444"}"#),
            );
            // Installed on one handle, observed on the other — this is what makes
            // an in-flight Extend see a revocation the instant it lands.
            store.install(&env, NOW, 0).unwrap();
            assert_eq!(
                clone.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Revoked
            );
            assert_eq!(clone.seq(), Some(11));
        }
    }
}
