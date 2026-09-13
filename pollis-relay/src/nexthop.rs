//! Which addresses this node will open a circuit's next leg to (#813, the
//! `Extend` half of the closed-overlay guarantee).
//!
//! `Connect` has always been bounded by the static destination allowlist
//! ([`crate::server::Allowlist`]). `Extend` had no equivalent: the frame carries
//! a bare `SocketAddr` plus a SHA-256 the client also chooses, so a node that
//! honoured it would open a QUIC connection to **any** address a client named —
//! a relay tier that dials arbitrary hosts on request, on first-party machines
//! and on every volunteer's device. The pinned fingerprint bounds who may *talk
//! back*; it does not bound who gets *dialled*, and the dial itself is the
//! primitive (an internal port scanner, a reflector, a way to make someone
//! else's machine originate the packet).
//!
//! So the next hop is bounded the same way a destination is — by policy, checked
//! before a socket is opened:
//!
//! 1. **It must be in the currently verified signed directory.** The pool's
//!    Ed25519-signed directory is the only statement of what a relay *is*
//!    (design §3/§7), and it is the same artifact the client picks paths from —
//!    so this refuses exactly the hops no honest client would ever name.
//! 2. **It must be a public unicast address** ([`is_public_dial_address`]) —
//!    never loopback, RFC1918, CGNAT, link-local, unique-local, multicast,
//!    broadcast or unspecified. Redundant while (1) holds, and deliberately so:
//!    it bounds the blast radius of a mis-signed directory to "relays that do
//!    not answer" instead of "an intranet scanner in every volunteer's home".
//! 3. **It must not be this node.** The directory entry carrying our own leaf
//!    names our own address; extending to it is a loop, not a hop.
//!
//! [`NextHopDirectory`] is fail-closed in exactly the shape
//! [`crate::policy::RevocationStore`] is, and for the same reason: a node with
//! no pinned directory key, or with no fresh directory installed, permits
//! **nothing**, so it simply stops being a middle hop. A permissive default is
//! how a check like this quietly stops meaning anything.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// The `type` discriminator a directory payload carries, matching
/// `pollis_core::net::directory::DIRECTORY_TYPE` and the JS reference
/// (`infra/relay-hydra/lib/directory-verify.mjs`).
///
/// Absence is accepted for the same reason the client accepts it: the
/// reconciler published directories before the field existed, and requiring it
/// would couple this build to a Lambda release with no safe ordering. A
/// present-and-wrong `type` is a hard reject — that is the cross-artifact
/// confusion guard, since the same key also signs `revocations.json`.
pub const DIRECTORY_TYPE: &str = "pollis-relay-directory";

/// The only directory payload version this build understands.
pub const DIRECTORY_VERSION: u32 = 1;

/// Why a directory was rejected. Every variant is fail-closed: a rejected
/// directory never becomes evidence that an address is dialable, it leaves the
/// previously-held one in place, and a node holding none extends to nothing.
#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("malformed directory envelope JSON")]
    MalformedEnvelope,
    #[error("malformed directory payload JSON")]
    MalformedPayload,
    #[error("bad base64 in {0}")]
    BadBase64(&'static str),
    #[error("pinned directory key is not a 32-byte Ed25519 public key")]
    BadPinnedKey,
    #[error("signature is not 64 bytes")]
    BadSignatureLen,
    #[error("bad signature")]
    BadSignature,
    #[error("unsupported directory version {0}")]
    UnsupportedVersion(u32),
    #[error("wrong artifact type {0:?} (expected {DIRECTORY_TYPE:?} or none)")]
    WrongType(String),
    #[error("directory expired (now {now} >= expires_at {expires_at})")]
    Expired { now: i64, expires_at: i64 },
    #[error("directory lists no relays")]
    EmptyRelays,
    #[error("directory rolled back (issued_at {issued_at} < held {held})")]
    RolledBack { issued_at: i64, held: i64 },
    /// No pinned directory key was configured, so nothing can be verified.
    #[error("no pinned directory key configured — the directory cannot be evaluated")]
    NotConfigured,
}

/// The signed envelope, byte-identical in shape to the revocation list's.
#[derive(Deserialize)]
struct Envelope {
    payload_b64: String,
    signature_b64: String,
}

/// Only the fields a *relay* needs. The client's [`Directory`] carries more
/// (regions, the revocation anchor); parsing a subset here keeps the relay's
/// copy small, and `serde` ignores the rest.
///
/// [`Directory`]: https://docs.rs/pollis-core
#[derive(Deserialize)]
struct DirectoryPayload {
    version: u32,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    issued_at: i64,
    expires_at: i64,
    /// Defaulted so that a payload carrying no `relays` at all reaches the
    /// `type` guard below and is rejected as the foreign artifact it is, rather
    /// than as malformed JSON. A directory that really lists none is rejected a
    /// line later, by [`DirectoryError::EmptyRelays`].
    #[serde(default)]
    relays: Vec<PayloadRelay>,
    #[serde(default)]
    peers: Vec<PayloadPeer>,
}

#[derive(Deserialize)]
struct PayloadRelay {
    addr: String,
    #[serde(default)]
    cert_b64: String,
}

#[derive(Deserialize)]
struct PayloadPeer {
    /// The first-party relays this peer is parked at. A peer has no address of
    /// its own — it is reached by splicing into the connection it opened — so
    /// these are the only dialable addresses a peer entry contributes.
    #[serde(default)]
    parked_at: Vec<String>,
}

/// One dialable next hop the directory names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NextHop {
    addr: SocketAddr,
    /// The DER leaf the directory pins for this address, empty for an address
    /// that only appeared in a peer's `parked_at` list. Used solely to
    /// recognise **this** node's own entry.
    cert_der: Vec<u8>,
}

/// A verified, unexpired directory reduced to the question a relay asks of it:
/// "may I dial this address?"
///
/// Construct one only via [`verify_next_hops`], so holding one is itself the
/// proof that the signature, version, type and expiry checks passed.
#[derive(Debug, Clone)]
pub struct NextHops {
    issued_at: i64,
    expires_at: i64,
    hops: Vec<NextHop>,
}

impl NextHops {
    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    /// How many distinct addresses this directory makes dialable.
    pub fn len(&self) -> usize {
        self.hops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }

    /// Is `now_secs` still inside this directory's validity window?
    pub fn is_fresh(&self, now_secs: i64) -> bool {
        now_secs < self.expires_at
    }

    /// The address of the entry pinned to `own_leaf_der`, i.e. this node's own
    /// place in the pool — `None` when the directory does not list us.
    fn own_addr(&self, own_leaf_der: &[u8]) -> Option<SocketAddr> {
        if own_leaf_der.is_empty() {
            return None;
        }
        self.hops
            .iter()
            .find(|hop| hop.cert_der == own_leaf_der)
            .map(|hop| hop.addr)
    }

    fn lists(&self, addr: SocketAddr) -> bool {
        self.hops.iter().any(|hop| hop.addr == addr)
    }
}

/// Why a next hop was refused — one variant per refusal class, so an operator
/// (and a test) can tell "the client named a hop we do not know" from "the
/// client named the loopback interface".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialVerdict {
    /// A fresh signed directory lists this address and it is dialable.
    Allow,
    /// **No usable evidence**: no pinned key, no directory installed, or the
    /// one held has expired. Not a permission — the node stops extending.
    Unevaluable,
    /// Verified directory, but it does not name this address.
    NotInDirectory,
    /// Loopback, private, CGNAT, link-local, unique-local, multicast,
    /// broadcast, unspecified, or port 0 — never a relay, always an attempt to
    /// use this node as a dialer.
    NotPublic,
    /// The address the directory pins to this node's own leaf. A hop to
    /// ourselves is a loop.
    SelfDial,
}

impl DialVerdict {
    /// The single place the fail-closed rule is encoded: only
    /// [`DialVerdict::Allow`] opens a socket.
    pub fn allowed(self) -> bool {
        matches!(self, DialVerdict::Allow)
    }

    /// A short, log-safe reason (an aggregate class, never an address).
    pub fn reason(self) -> &'static str {
        match self {
            DialVerdict::Allow => "allowed",
            DialVerdict::Unevaluable => "no fresh signed directory to check against",
            DialVerdict::NotInDirectory => "not listed in the signed directory",
            DialVerdict::NotPublic => "not a public unicast address",
            DialVerdict::SelfDial => "is this node itself",
        }
    }
}

/// Is `addr` an address a relay could legitimately be published at?
///
/// Everything a hosting provider hands out publicly passes; everything that
/// only resolves to something on the *dialer's* side of the network does not.
/// IPv4-mapped IPv6 (`::ffff:127.0.0.1`) is unwrapped first — otherwise it is a
/// one-cast bypass of every IPv4 rule below.
pub fn is_public_dial_address(addr: &SocketAddr) -> bool {
    if addr.port() == 0 {
        return false;
    }
    match addr.ip() {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(ip),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    // `is_shared` (CGNAT, 100.64.0.0/10) is still unstable, so the range is
    // spelled out rather than waited for. The documentation ranges are NOT
    // refused: they are unroutable rather than local, so they reach nothing on
    // the dialer's side of the network — which is the whole thing this refuses.
    let shared = ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 64;
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || shared)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    // fc00::/7 unique-local and fe80::/10 link-local; both predicates are
    // unstable in std, and both are exactly the "only reachable from where the
    // dialer sits" shape this function exists to refuse.
    let unique_local = (segments[0] & 0xfe00) == 0xfc00;
    let link_local = (segments[0] & 0xffc0) == 0xfe80;
    !(ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || unique_local || link_local)
}

/// Verify a directory envelope against the pinned directory-signing key and
/// reduce it to the dialable next hops it names.
///
/// The accept set is deliberately **identical** to the client's
/// (`pollis_core::net::directory::verify_directory`): signature over the exact
/// decoded payload bytes first, then `version == 1`, then the `type` guard,
/// then expiry, then non-empty `relays`. A relay that rejected a directory the
/// fleet accepts would silently stop carrying multi-hop traffic.
pub fn verify_next_hops(
    envelope_bytes: &[u8],
    pinned_pubkey_b64: &str,
    now_secs: i64,
) -> Result<NextHops, DirectoryError> {
    let envelope: Envelope =
        serde_json::from_slice(envelope_bytes).map_err(|_| DirectoryError::MalformedEnvelope)?;

    let payload = B64
        .decode(envelope.payload_b64.as_bytes())
        .map_err(|_| DirectoryError::BadBase64("payload_b64"))?;
    let signature_bytes = B64
        .decode(envelope.signature_b64.as_bytes())
        .map_err(|_| DirectoryError::BadBase64("signature_b64"))?;

    let verifying_key = verifying_key_from_b64(pinned_pubkey_b64)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| DirectoryError::BadSignatureLen)?;
    verifying_key
        .verify_strict(&payload, &signature)
        .map_err(|_| DirectoryError::BadSignature)?;

    let payload: DirectoryPayload =
        serde_json::from_slice(&payload).map_err(|_| DirectoryError::MalformedPayload)?;

    if payload.version != DIRECTORY_VERSION {
        return Err(DirectoryError::UnsupportedVersion(payload.version));
    }
    if let Some(kind) = payload.kind.as_deref() {
        if kind != DIRECTORY_TYPE {
            return Err(DirectoryError::WrongType(kind.to_string()));
        }
    }
    if now_secs >= payload.expires_at {
        return Err(DirectoryError::Expired {
            now: now_secs,
            expires_at: payload.expires_at,
        });
    }
    if payload.relays.is_empty() {
        return Err(DirectoryError::EmptyRelays);
    }

    // An entry whose address will not parse contributes no dialable hop. That
    // can only ever REMOVE a permission, so a directory carrying a hostname a
    // future producer starts publishing degrades to "that relay is not a next
    // hop here", never to "dial whatever this string resolves to".
    let mut hops: Vec<NextHop> = Vec::with_capacity(payload.relays.len());
    let mut seen: HashSet<SocketAddr> = HashSet::new();
    for relay in &payload.relays {
        let Ok(addr) = relay.addr.trim().parse::<SocketAddr>() else {
            continue;
        };
        let cert_der = B64.decode(relay.cert_b64.trim().as_bytes()).unwrap_or_default();
        if seen.insert(addr) {
            hops.push(NextHop { addr, cert_der });
        }
    }
    for peer in &payload.peers {
        for parked_at in &peer.parked_at {
            let Ok(addr) = parked_at.trim().parse::<SocketAddr>() else {
                continue;
            };
            if seen.insert(addr) {
                hops.push(NextHop {
                    addr,
                    cert_der: Vec::new(),
                });
            }
        }
    }

    Ok(NextHops {
        issued_at: payload.issued_at,
        expires_at: payload.expires_at,
        hops,
    })
}

fn verifying_key_from_b64(pinned_pubkey_b64: &str) -> Result<VerifyingKey, DirectoryError> {
    let raw = B64
        .decode(pinned_pubkey_b64.trim().as_bytes())
        .map_err(|_| DirectoryError::BadBase64("pinned key"))?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| DirectoryError::BadPinnedKey)?;
    VerifyingKey::from_bytes(&raw).map_err(|_| DirectoryError::BadPinnedKey)
}

/// The live directory a relay checks `Extend` against.
///
/// Cheap to clone — every clone shares one cell, so a directory installed by a
/// refresh task is observed immediately by every in-flight `Extend`, and a
/// volunteer's engine and its reachability loop can hold the same store.
///
/// **Fail-closed by construction:**
/// - no pinned key ([`NextHopDirectory::unconfigured`]) ⇒ nothing is dialable;
/// - no directory installed yet ⇒ nothing is dialable;
/// - the held directory has expired ⇒ nothing is dialable;
/// - [`NextHopDirectory::install`] never accepts a directory issued before the
///   one it holds, so a captured older-but-unexpired directory cannot be
///   replayed to restore a relay that has since left the pool.
#[derive(Clone)]
pub struct NextHopDirectory {
    inner: Arc<RwLock<Option<NextHops>>>,
    /// `None` ⇒ this node cannot evaluate the directory at all.
    pinned_key_b64: Option<Arc<String>>,
}

impl NextHopDirectory {
    fn with_key(pinned_key_b64: Option<Arc<String>>) -> NextHopDirectory {
        NextHopDirectory {
            inner: Arc::new(RwLock::new(None)),
            pinned_key_b64,
        }
    }

    /// A store that verifies against `pinned_pubkey_b64` — the same
    /// `POLLIS_OVERLAY_DIRECTORY_KEY` that pins the directory and the
    /// revocation list.
    pub fn enforcing(pinned_pubkey_b64: impl Into<String>) -> NextHopDirectory {
        NextHopDirectory::with_key(Some(Arc::new(pinned_pubkey_b64.into())))
    }

    /// A store for a node that was never given a directory key. It permits
    /// nothing, so the node serves circuits that terminate on it and refuses
    /// every `Extend` — the same strict state an unconfigured
    /// [`crate::policy::RevocationStore`] puts it in.
    pub fn unconfigured() -> NextHopDirectory {
        NextHopDirectory::with_key(None)
    }

    /// Can this store evaluate a next hop at all?
    pub fn is_configured(&self) -> bool {
        self.pinned_key_b64.is_some()
    }

    /// Verify `envelope_bytes` and adopt it if it is not older than what we
    /// hold. Returns how many dialable hops the directory names.
    pub fn install(&self, envelope_bytes: &[u8], now_secs: i64) -> Result<usize, DirectoryError> {
        let key = self
            .pinned_key_b64
            .as_ref()
            .ok_or(DirectoryError::NotConfigured)?;
        let next = verify_next_hops(envelope_bytes, key, now_secs)?;
        let mut guard = self.write();
        if let Some(held) = guard.as_ref() {
            if next.issued_at < held.issued_at {
                return Err(DirectoryError::RolledBack {
                    issued_at: next.issued_at,
                    held: held.issued_at,
                });
            }
        }
        let len = next.len();
        *guard = Some(next);
        Ok(len)
    }

    /// When the held directory expires, if one is held.
    pub fn expires_at(&self) -> Option<i64> {
        self.read().as_ref().map(NextHops::expires_at)
    }

    /// Decide whether this node may open a circuit's next leg to `addr`.
    ///
    /// `own_leaf_der` is this node's own QUIC leaf, which is how its own
    /// directory entry is recognised. `allow_private` relaxes only rule (2) —
    /// it exists for loopback test rigs and lab pools, and never lets an
    /// address the directory does not name through.
    pub fn verdict(
        &self,
        addr: SocketAddr,
        own_leaf_der: &[u8],
        now_secs: i64,
        allow_private: bool,
    ) -> DialVerdict {
        if !allow_private && !is_public_dial_address(&addr) {
            return DialVerdict::NotPublic;
        }
        if self.pinned_key_b64.is_none() {
            return DialVerdict::Unevaluable;
        }
        let guard = self.read();
        let Some(directory) = guard.as_ref() else {
            return DialVerdict::Unevaluable;
        };
        // Freshness at USE time, not just at install time: a directory that has
        // lapsed is no longer a statement about who is in the pool, and treating
        // it as one is the whole failure mode the expiry exists to prevent.
        if !directory.is_fresh(now_secs) {
            return DialVerdict::Unevaluable;
        }
        if directory.own_addr(own_leaf_der) == Some(addr) {
            return DialVerdict::SelfDial;
        }
        if !directory.lists(addr) {
            return DialVerdict::NotInDirectory;
        }
        DialVerdict::Allow
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<NextHops>> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<NextHops>> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for NextHopDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.read();
        f.debug_struct("NextHopDirectory")
            .field("configured", &self.pinned_key_b64.is_some())
            .field("hops", &guard.as_ref().map(NextHops::len))
            .field("expires_at", &guard.as_ref().map(NextHops::expires_at))
            .finish()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Keeping a deployed node's directory loaded
// ─────────────────────────────────────────────────────────────────────────────

/// How long a single directory fetch may take before it is abandoned.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Cap on the artifact size we will read, matching
/// [`crate::revocation_sync`]'s: the published directory is a few KB, and a
/// hostile origin does not get to feed a node an unbounded body.
const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

/// Fetch the signed directory once and install it. Every failure mode leaves
/// the store holding whatever it held before — which expires on its own.
pub async fn refresh_once(store: &NextHopDirectory, url: &str) -> anyhow::Result<usize> {
    let response = crate::http::http_client(None)
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("fetch {url}: HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("read {url}: {e}"))?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(anyhow::anyhow!(
            "directory from {url} is {} bytes, over the {MAX_ARTIFACT_BYTES} cap",
            bytes.len()
        ));
    }
    let hops = store.install(&bytes, crate::proto::now_unix())?;
    Ok(hops)
}

/// Run the directory refresh loop until `shutdown` resolves, on the same
/// cadence and backoff as [`crate::revocation_sync::spawn`] — and for the same
/// reason it is allowed to be a loop: the artifact this node must hold is
/// short-lived and checked at use time, so there is no event to be driven by,
/// and a missed refresh degrades the node to "no longer a middle hop" rather
/// than to "extending on a stale directory".
///
/// Spawns nothing for an unconfigured store: there is nothing to install into
/// one, and it is already in its strictest state.
pub fn spawn<F>(
    store: NextHopDirectory,
    url: String,
    shutdown: F,
) -> Option<tokio::task::JoinHandle<()>>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if !store.is_configured() {
        tracing::warn!(
            "directory refresh not started: no pinned directory key — this node will refuse to extend circuits"
        );
        return None;
    }
    Some(tokio::spawn(async move {
        tokio::pin!(shutdown);
        let mut failures: u32 = 0;
        loop {
            match refresh_once(&store, &url).await {
                Ok(hops) => {
                    tracing::debug!("relay directory refreshed: {hops} dialable next hops");
                    failures = 0;
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!("directory refresh failed ({failures} in a row): {e}");
                }
            }
            let delay = crate::backoff::jittered(crate::revocation_sync::backoff_after(failures));
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    break;
                }
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    fn pinned() -> String {
        B64.encode(key().verifying_key().to_bytes())
    }

    fn envelope(payload: &str) -> Vec<u8> {
        let signature = key().sign(payload.as_bytes());
        serde_json::to_vec(&serde_json::json!({
            "payload_b64": B64.encode(payload.as_bytes()),
            "signature_b64": B64.encode(signature.to_bytes()),
        }))
        .unwrap()
    }

    fn directory(relays: &str, now: i64) -> Vec<u8> {
        envelope(&format!(
            r#"{{"version":1,"type":"pollis-relay-directory","issued_at":{now},"expires_at":{},"relays":[{relays}]}}"#,
            now + 3600
        ))
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn an_unconfigured_store_permits_nothing() {
        let store = NextHopDirectory::unconfigured();
        assert!(!store.is_configured());
        assert_eq!(
            store.verdict(addr("203.0.113.7:9444"), b"leaf", 1000, false),
            DialVerdict::Unevaluable
        );
        // ...and there is no way to load one, so it cannot drift open.
        assert!(store.install(&directory("", 1000), 1000).is_err());
    }

    #[test]
    fn a_configured_store_with_no_directory_permits_nothing() {
        let store = NextHopDirectory::enforcing(pinned());
        assert_eq!(
            store.verdict(addr("203.0.113.7:9444"), b"leaf", 1000, false),
            DialVerdict::Unevaluable
        );
    }

    #[test]
    fn a_listed_address_is_allowed_and_an_unlisted_one_is_not() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        store
            .install(
                &directory(r#"{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}"#, now),
                now,
            )
            .expect("a freshly-signed directory installs");
        assert_eq!(
            store.verdict(addr("203.0.113.7:9444"), b"leaf", now, false),
            DialVerdict::Allow
        );
        // Same host, different port: a different endpoint, not a relay we know.
        assert_eq!(
            store.verdict(addr("203.0.113.7:22"), b"leaf", now, false),
            DialVerdict::NotInDirectory
        );
        assert_eq!(
            store.verdict(addr("198.51.100.9:9444"), b"leaf", now, false),
            DialVerdict::NotInDirectory
        );
    }

    #[test]
    fn a_peers_parked_at_relay_is_dialable_but_the_peer_itself_has_no_address() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        let payload = format!(
            r#"{{"version":1,"issued_at":{now},"expires_at":{},"relays":[{{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}}],"peers":[{{"cert_b64":"UEVFUg==","parked_at":["198.51.100.9:9444"]}}]}}"#,
            now + 3600
        );
        store.install(&envelope(&payload), now).unwrap();
        assert_eq!(
            store.verdict(addr("198.51.100.9:9444"), b"leaf", now, false),
            DialVerdict::Allow
        );
    }

    #[test]
    fn an_expired_directory_stops_permitting_at_use_time() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        store
            .install(
                &directory(r#"{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}"#, now),
                now,
            )
            .unwrap();
        assert_eq!(
            store.verdict(addr("203.0.113.7:9444"), b"leaf", now + 3600, false),
            DialVerdict::Unevaluable
        );
    }

    #[test]
    fn this_nodes_own_entry_is_never_a_next_hop() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        let own = b"my-own-leaf-der";
        store
            .install(
                &directory(
                    &format!(
                        r#"{{"addr":"203.0.113.7:9444","cert_b64":"{}"}},{{"addr":"198.51.100.9:9444","cert_b64":"QUJD"}}"#,
                        B64.encode(own)
                    ),
                    now,
                ),
                now,
            )
            .unwrap();
        assert_eq!(
            store.verdict(addr("203.0.113.7:9444"), own, now, false),
            DialVerdict::SelfDial
        );
        // Every other listed relay is still a legitimate hop.
        assert_eq!(
            store.verdict(addr("198.51.100.9:9444"), own, now, false),
            DialVerdict::Allow
        );
    }

    #[test]
    fn private_and_local_addresses_are_refused_even_when_listed() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        let listed = [
            "127.0.0.1:9444",
            "10.0.0.5:9444",
            "172.16.4.4:9444",
            "192.168.1.1:9444",
            "169.254.169.254:80",
            "100.64.0.1:9444",
            "224.0.0.1:9444",
            "255.255.255.255:9444",
            "0.0.0.0:9444",
            "[::1]:9444",
            "[fe80::1]:9444",
            "[fc00::1]:9444",
            "[::ffff:127.0.0.1]:9444",
        ];
        let relays = listed
            .iter()
            .map(|a| format!(r#"{{"addr":"{a}","cert_b64":"QUJD"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        store.install(&directory(&relays, now), now).unwrap();
        for a in listed {
            assert_eq!(
                store.verdict(addr(a), b"leaf", now, false),
                DialVerdict::NotPublic,
                "{a} must never be dialable"
            );
        }
        // Port 0 is not an endpoint either.
        assert_eq!(
            store.verdict(addr("203.0.113.7:0"), b"leaf", now, false),
            DialVerdict::NotPublic
        );
    }

    #[test]
    fn allow_private_relaxes_the_address_class_and_nothing_else() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        store
            .install(
                &directory(r#"{"addr":"127.0.0.1:9444","cert_b64":"QUJD"}"#, now),
                now,
            )
            .unwrap();
        assert_eq!(
            store.verdict(addr("127.0.0.1:9444"), b"leaf", now, true),
            DialVerdict::Allow
        );
        // Still gated on the directory: the lab switch is not an open door.
        assert_eq!(
            store.verdict(addr("127.0.0.1:9445"), b"leaf", now, true),
            DialVerdict::NotInDirectory
        );
    }

    #[test]
    fn a_forged_or_foreign_artifact_is_refused() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        // Signed by someone else.
        let other = SigningKey::from_bytes(&[1u8; 32]);
        let payload = format!(
            r#"{{"version":1,"issued_at":{now},"expires_at":{},"relays":[{{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}}]}}"#,
            now + 3600
        );
        let forged = serde_json::to_vec(&serde_json::json!({
            "payload_b64": B64.encode(payload.as_bytes()),
            "signature_b64": B64.encode(other.sign(payload.as_bytes()).to_bytes()),
        }))
        .unwrap();
        assert!(matches!(
            store.install(&forged, now),
            Err(DirectoryError::BadSignature)
        ));
        // The revocation list is signed by the SAME key — it must not be
        // mistakable for a directory.
        let revocations = envelope(&format!(
            r#"{{"version":1,"type":"pollis-relay-revocations","seq":1,"issued_at":{now},"expires_at":{},"revoked":[]}}"#,
            now + 300
        ));
        assert!(matches!(
            store.install(&revocations, now),
            Err(DirectoryError::WrongType(_))
        ));
        // And nothing landed: the store still permits nothing.
        assert_eq!(
            store.verdict(addr("203.0.113.7:9444"), b"leaf", now, false),
            DialVerdict::Unevaluable
        );
    }

    #[test]
    fn an_older_directory_cannot_replace_a_newer_one() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        store
            .install(
                &directory(r#"{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}"#, now),
                now,
            )
            .unwrap();
        // A captured, still-unexpired directory that listed a relay since removed.
        let older = directory(r#"{"addr":"198.51.100.9:9444","cert_b64":"QUJD"}"#, now - 60);
        assert!(matches!(
            store.install(&older, now),
            Err(DirectoryError::RolledBack { .. })
        ));
        assert_eq!(
            store.verdict(addr("198.51.100.9:9444"), b"leaf", now, false),
            DialVerdict::NotInDirectory
        );
    }

    #[test]
    fn an_empty_or_wrong_version_directory_is_refused() {
        let now = 1_700_000_000;
        let store = NextHopDirectory::enforcing(pinned());
        assert!(matches!(
            store.install(&directory("", now), now),
            Err(DirectoryError::EmptyRelays)
        ));
        let v2 = envelope(&format!(
            r#"{{"version":2,"issued_at":{now},"expires_at":{},"relays":[{{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}}]}}"#,
            now + 3600
        ));
        assert!(matches!(
            store.install(&v2, now),
            Err(DirectoryError::UnsupportedVersion(2))
        ));
    }
}
