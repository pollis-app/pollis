//! Per-target routing policy (overlay / direct / degraded, design §6.4) and
//! live relay revocation. Both are pure, no I/O.
//!
//! Revocation exists because the signed directory has a ~1 hour TTL, so a
//! seized relay stays "valid" until it lapses; the revocation list is a second,
//! shorter-lived signed artifact shared by client path selection and relay
//! `Extend` checks.

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
    /// Encoding for the live-switchable [`AtomicU8`] mode cell.
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

/// Decides how each target is routed, given the mode, the overlay
/// (control-plane) hosts and the always-direct (media) hosts.
///
/// `mode` is a shared [`AtomicU8`] so a running shim can switch Prefer↔Strict
/// without a restart ([`OverlayHandle::set_mode`](crate::shim::OverlayHandle::set_mode)).
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

    /// Flip the live routing mode; the shim's next `plan()` observes it.
    pub fn set_mode(&self, mode: OverlayMode) {
        self.mode.store(mode.as_u8(), Ordering::Relaxed);
    }

    pub(crate) fn mode_atomic(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.mode)
    }

    // Routing is per host; the port is judged by the relay's egress allowlist.
    fn is_direct_host(&self, host: &str) -> bool {
        self.direct_hosts.iter().any(|p| p.matches_host(host))
    }

    fn is_overlay_host(&self, host: &str) -> bool {
        self.overlay_hosts.iter().any(|p| p.matches_host(host))
    }

    /// Decide the intended route for `host`.
    ///
    /// - Media/direct hosts → Direct in every mode.
    /// - `Off` → Direct for everything.
    /// - Control-plane host + `Prefer` → Overlay (direct fallback).
    /// - Control-plane host + `Strict` → Overlay (degrade on failure).
    /// - Any other host → Direct (e.g. non-first-party like Expo push, §14.4).
    pub fn plan(&self, host: &str) -> PlannedRoute {
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

/// The `type` discriminator every revocation payload must carry (#813).
///
/// The directory and the revocation list are signed by the same key, so
/// without it one could be replayed as the other; a mismatch is a hard reject.
pub const REVOCATION_TYPE: &str = "pollis-relay-revocations";

/// The only revocation payload version this build understands.
pub const REVOCATION_VERSION: u32 = 1;

/// Why a revocation list was rejected. Every variant is fail-closed: a rejected
/// list leaves [`RevocationStore::admit`] answering [`Admission::Unevaluable`].
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

/// Same shape as the directory envelope: the Ed25519 signature covers exactly
/// the decoded payload bytes, with no canonicalization.
#[derive(Deserialize)]
struct RevocationEnvelope {
    payload_b64: String,
    signature_b64: String,
}

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

/// One revocation. An entry carrying neither selector is rejected
/// ([`RevocationError::Unenforceable`]).
#[derive(Deserialize)]
struct RevokedEntry {
    #[serde(default)]
    addr: Option<String>,
    #[serde(default)]
    cert_sha256_b64: Option<String>,
}

/// A verified, unexpired, non-rolled-back revocation list. Only
/// [`verify_revocations`] constructs one, so holding it proves the checks passed.
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
    /// The publisher's monotonic sequence number (see `infra/relay-hydra`).
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    /// How many relays this list revokes. Empty is legal and the steady state.
    pub fn len(&self) -> usize {
        self.addrs.len() + self.cert_digests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_fresh(&self, now_secs: i64) -> bool {
        now_secs < self.expires_at
    }

    /// Either selector matching revokes. Address is needed because hydra nodes
    /// share one pinned leaf (a cert revocation would drop the whole pool); the
    /// cert digest covers per-node identities (§7).
    pub fn revokes(&self, id: &RelayIdentity<'_>) -> bool {
        if self.addrs.contains(&normalize_addr(id.addr)) {
            return true;
        }
        self.cert_digests.contains(&cert_digest_b64(id.cert_der))
    }
}

/// The relay being checked: the advertised endpoint plus the DER leaf it
/// presented on the wire.
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
    /// No usable evidence (never loaded, expired, or unconfigured). Not an
    /// admission.
    Unevaluable,
}

impl Admission {
    /// The fail-closed rule: only [`Admission::Admit`] is an admission; "unknown"
    /// and "revoked" are the same answer.
    pub fn admitted(self) -> bool {
        matches!(self, Admission::Admit)
    }
}

/// Verify a revocation envelope against the pinned directory-signing key:
/// signature over the payload bytes first, then parse and range-check. Must
/// match `infra/relay-hydra/lib/revocation-verify.mjs`.
///
/// `seq_floor`: the greater of the highest seq already seen (rollback) and the
/// seq the signed directory anchored (freshness). Below it is a replay.
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
        // An entry this build cannot act on fails the whole list closed: skipping
        // it would turn a newer producer's revocation into an admission.
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
            // A non-32-byte digest can never match, so it is unenforceable.
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
/// Clones share one cell, so a new list is seen by every in-flight `Extend`.
///
/// Fail-closed: no pinned key, no installed list, or an expired list admits
/// nothing, and [`RevocationStore::install`] never accepts a seq below the
/// high-water mark (no un-revoking by replay).
#[derive(Clone)]
pub struct RevocationStore {
    inner: Arc<RwLock<StoreInner>>,
    /// `None` ⇒ this node cannot evaluate revocation at all.
    pinned_key_b64: Option<Arc<String>>,
}

struct StoreInner {
    list: Option<RevocationList>,
    /// Highest sequence ever accepted; monotonic, survives rejected installs.
    high_water: u64,
}

impl RevocationStore {
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

    /// A store for a node with no directory key. Not a bypass: it admits
    /// nothing, so such a relay cannot `Extend` (single-hop `CONNECT` still works).
    pub fn unconfigured() -> RevocationStore {
        RevocationStore::with_key(None)
    }

    pub fn is_configured(&self) -> bool {
        self.pinned_key_b64.is_some()
    }

    /// Verify `envelope_bytes` and adopt it if newer-or-equal; returns its seq.
    ///
    /// `directory_seq_floor` (the seq the signed directory anchored) stops an
    /// on-path attacker pairing a fresh directory with a stale-but-unexpired list.
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
        // Re-check under the write lock so a concurrent older install can't win.
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

    pub fn seq(&self) -> Option<u64> {
        self.read().list.as_ref().map(RevocationList::seq)
    }

    pub fn high_water(&self) -> u64 {
        self.read().high_water
    }

    pub fn admit(&self, id: &RelayIdentity<'_>, now_secs: i64) -> Admission {
        if self.pinned_key_b64.is_none() {
            return Admission::Unevaluable;
        }
        let guard = self.read();
        let Some(list) = guard.list.as_ref() else {
            return Admission::Unevaluable;
        };
        // Staleness is checked at use time, not only at install time.
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
        assert_eq!(
            policy.plan("control.test"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        policy.set_mode(OverlayMode::Strict);
        assert_eq!(
            policy.plan("control.test"),
            PlannedRoute::Overlay { fallback_to_direct: false }
        );
        let handle_cell = policy.mode_handle_for_test();
        handle_cell.store(OverlayMode::Off.as_u8(), Ordering::Relaxed);
        assert_eq!(policy.mode(), OverlayMode::Off);
        assert_eq!(policy.plan("control.test"), PlannedRoute::Direct);
    }

    mod revocation {
        use super::*;
        use ed25519_dalek::{Signer, SigningKey};

        const NOW: i64 = 1_737_600_000;
        const CERT: &[u8] = b"a relay leaf, DER in real life";
        const OTHER_CERT: &[u8] = b"a different relay leaf";

        fn signer() -> SigningKey {
            SigningKey::from_bytes(&[3u8; 32])
        }

        fn attacker() -> SigningKey {
            SigningKey::from_bytes(&[4u8; 32])
        }

        fn pinned(sk: &SigningKey) -> String {
            B64.encode(sk.verifying_key().to_bytes())
        }

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
            assert_eq!(
                store.admit(&id(" 203.0.113.7:9444 ", CERT), NOW),
                Admission::Revoked
            );
            // Another node sharing the same pinned leaf is untouched.
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
            let forged = envelope(&attacker(), &payload(9, NOW + 300, ""));
            let store = RevocationStore::enforcing(pinned(&sk));
            assert!(matches!(
                store.install(&forged, NOW, 0),
                Err(RevocationError::BadSignature)
            ));
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
            // Strip the revocation but keep the old signature.
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

            assert_eq!(
                store.admit(&id("203.0.113.7:9444", CERT), NOW + 300),
                Admission::Unevaluable
            );
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
            store.install(&env, NOW, 0).unwrap();
            assert_eq!(
                clone.admit(&id("203.0.113.7:9444", CERT), NOW),
                Admission::Revoked
            );
            assert_eq!(clone.seq(), Some(11));
        }
    }
}
