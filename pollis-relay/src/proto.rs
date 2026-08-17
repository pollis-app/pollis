//! The shared relay wire protocol + the device-certificate handshake.
//!
//! # Framing
//!
//! Everything is length-prefixed and versioned with a 1-byte protocol version so
//! a future revision can be told apart on the first byte. Multi-byte integers are
//! big-endian. Strings are UTF-8, prefixed with a `u16` byte length. The client
//! opens one QUIC bi-stream per target and speaks, in order:
//!
//! ```text
//! ┌──────────────── Handshake (client → relay) ────────────────┐
//! │ u8     version          (3 or 4)                           │
//! │ u8     msg_type         (== MSG_HANDSHAKE)                 │
//! │ u16    user_id_len | user_id bytes          (UTF-8)        │
//! │ u16    device_id_len | device_id bytes      (UTF-8)        │
//! │ [32]   device_ed25519_pub (classic leaf, PRESENTED)        │
//! │ [1312] device_mldsa_pub   (ML-DSA-44 leaf, PRESENTED)      │
//! │ [1312] account_id_pub     (ML-DSA-44 account identity key) │
//! │ u32    identity_version   (big-endian)                     │
//! │ u64    issued_at          (big-endian, unix seconds)       │
//! │ [2420] device_cert        (account key's sig over the chain)│
//! │ i64    timestamp          (unix seconds, big-endian)       │
//! │ u8     nonce_len (== 32) | nonce bytes                     │
//! │ [2420] signature          (device ML-DSA key, canonical)   │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────────── Connect (client → relay) ──────────────────┐
//! │ u8   version            (3 or 4)                          │
//! │ u8   msg_type           (== MSG_CONNECT)                  │
//! │ u16  host_len | host bytes                  (UTF-8)       │
//! │ u16  port                                                 │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────────── Extend (client → relay, v4+) ──────────────┐
//! │ u8   version            (>= 4)                            │
//! │ u8   msg_type           (== MSG_EXTEND)                   │
//! │ u8   addr_family        (4 = IPv4, 6 = IPv6)              │
//! │ [4|16] next_hop_ip                                        │
//! │ u16  next_hop_port                                        │
//! │ [32] next_cert_sha256   (SHA-256 of the next hop's DER)   │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────────── Layer (relay → relay, v4+) ────────────────┐
//! │ u8   version            (>= 4)                            │
//! │ u8   msg_type           (== MSG_LAYER)                    │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────── Anchor (client → relay, v4+, optional) ────────┐
//! │ u8   version            (>= 4)                            │
//! │ u8   msg_type           (== MSG_ANCHOR)                   │
//! │ u16  leaf_len | leaf bytes    (account-key leaf, as hashed)│
//! │ u64  leaf_index                                           │
//! │ u64  tree_size          (of the head AND the proof)       │
//! │ u8   path_len | path_len × [32] sibling hashes            │
//! │ u64  sth_timestamp_ms                                     │
//! │ [32] sth_root                                             │
//! │ [2420] sth_signature    (ML-DSA-44, account-keys domain)  │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────── Park (peer → relay, v4+) ──────────────────────┐
//! │ u8   version            (>= 4)                            │
//! │ u8   msg_type           (== MSG_PARK)                     │
//! │ u16  der_len | relay_leaf_der bytes                       │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────────── Response (relay → client) ─────────────────┐
//! │ u8   version            (3 or 4)                          │
//! │ u8   status             (0 = Ok, 1 = Rejected)            │
//! │ -- if Rejected: --                                        │
//! │ u8   reason_code                                          │
//! │ u16  detail_len | detail bytes              (UTF-8)       │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! For a single-hop circuit the client pipelines Handshake + Connect without
//! waiting; the relay reads both, then sends exactly one Response communicating
//! the final outcome. On any rejection the relay closes the stream. After an
//! `Ok`, the stream becomes the raw byte pipe to the target.
//!
//! # Onion layering (v4, design §6.2)
//!
//! A multi-hop circuit is built by **telescoping**. Against hop *i* the client
//! sends Handshake + `Extend(hop i+1)`; that relay dials hop i+1 over QUIC,
//! announces itself with a `Layer` frame, and — once hop i+1 acknowledges —
//! splices the two streams and answers `Ok`. The client then runs **one TLS 1.3
//! session per hop, nested**, over that spliced pipe (see [`crate::onion`]),
//! pinning hop i+1's leaf cert. Every subsequent frame the client writes is
//! therefore wrapped in one encryption layer per remaining hop, and each relay
//! peels exactly one.
//!
//! Consequences, which are the whole point:
//! - hop 1 sees the client's address and hop 2's address — never the destination;
//! - a middle hop sees only its two neighbours;
//! - the last hop sees the destination and its predecessor — never the client's
//!   address. It still authenticates the client's *device cert*, exactly as the
//!   single relay does in v0, so the account is not hidden from it; the property
//!   this protocol delivers is **network-address unlinkability**, not anonymity.
//!
//! Hop-to-hop QUIC/TLS is unchanged and still carries everything; the onion
//! layer rides on top of it rather than replacing it.
//!
//! # Versioning
//!
//! [`PROTOCOL_VERSION`] is the newest generation this build speaks and
//! [`MIN_PROTOCOL_VERSION`] the oldest it still accepts. The ALPN token carries
//! the generation, and a relay offers **every** supported ALPN ([`SUPPORTED_ALPNS`],
//! newest first) so a v4 client keeps working against the shipped v3 fleet for
//! single-hop — it simply negotiates `pollis-relay/3` and writes `3` in every
//! frame's version byte. Anything outside the supported range fails the QUIC ALPN
//! negotiation outright, so a mismatched peer never gets as far as writing a
//! frame: versions never silently half-speak, and a stream is never corrupted.
//! `Extend` / `Layer` / `Anchor` additionally require version ≥ 4 at the frame
//! level, at both the writer and the reader, so a v3-negotiated connection can
//! smuggle neither a multi-hop request nor an account anchor.
//!
//! The Handshake frame and its canonical signed message are **byte-identical
//! between v3 and v4** — v4 is purely additive — which is exactly what lets one
//! client speak either generation with a single signing path. That is why
//! [`HANDSHAKE_DOMAIN`] is unchanged: its job is separation from other protocols,
//! not from other generations of this one.
//!
//! # Handshake auth — OFFLINE device-certificate chain (design §9.4, §11.1)
//!
//! Unlike Slice 1 (which resolved the device key from an in-memory table), the
//! client now **presents its full identity chain** and the relay verifies it with
//! **zero I/O** — no Turso query, no network call per connection. That is the
//! mechanism that keeps the relay tier out of the metadata plane (§11.1). Two
//! independent checks, both must pass:
//!
//! 1. **Possession.** The handshake `signature` verifies, under the presented
//!    `device_mldsa_pub`, over the canonical message (skew-bounded, nonce'd):
//!    ```text
//!    pollis-relay-v3\n{user_id}\n{device_id}\n{timestamp}\n{sha256_hex(nonce)}
//!    ```
//!    ⇒ the connecting party holds that device signing private key.
//! 2. **Membership.** [`pollis_device_cert::verify_device_cert`] confirms
//!    `device_cert` is the account key `account_id_pub`'s signature binding BOTH
//!    presented leaf keys to this `device_id` at `identity_version` / `issued_at`
//!    ⇒ those device keys were certified by the account.
//!
//! Every signature in the chain is ML-DSA-44 as of #668, so a store-now-decrypt-
//! later adversary with a CRQC cannot forge a relay identity retroactively. The
//! Ed25519 leaf key is still carried and still bound by the cert — it remains the
//! classic MLS suite's leaf key until #669 retires that suite — but nothing in
//! this handshake verifies under it.
//!
//! Together: "a cryptographically self-consistent Pollis device." Because
//! destinations are allowlisted to first-party hosts only (§1.2), "a well-formed
//! device, rate-limited" is sufficient anti-abuse for v0.
//!
//! # Account anchoring — the optional third check (v4+, #813 Phase E2)
//!
//! Self-consistency is not membership: an attacker can mint an account key and a
//! matching device cert offline, in a loop, and each one is a fresh per-account
//! rate-limiting identity. The `Anchor` frame closes that by having the client
//! **present** an inclusion proof binding its `account_id_pub` to the account-key
//! transparency log, which the relay verifies with **zero I/O** against one
//! pinned log key — see [`crate::anchor`] for the full argument and its limits.
//! A relay fetch was the alternative and is ruled out: it would put the relay
//! back inside the metadata plane, which is the coupling §11.1 removes.
//!
//! The frame is optional on the wire and gated at version ≥ 4 exactly as
//! `Extend` / `Layer` are, so a v3 stream can neither send one nor be required to.
//! Whether a relay *requires* one is node policy ([`crate::anchor::AnchorPolicy`]),
//! not protocol.
//!
//! # Parking — the reverse hop (v4+, #813 Phase D1/P1)
//!
//! A **peer-hosted** relay is a consumer device behind NAT: a first-party relay
//! cannot dial it, and neither end is an ICE agent, so the only connection that
//! can exist is the one the peer opens **outbound**. `Park` is how it offers
//! that connection as a middle hop: after the ordinary device handshake — the
//! same one every client runs, no new credential — the peer names the relay leaf
//! cert it serves under, and the relay keeps the connection open, keyed on
//! `sha256(relay_leaf_der)`. A later `Extend` naming that fingerprint is spliced
//! into the parked connection instead of dialling ([`crate::park`]).
//!
//! `Park` is gated at version ≥ 4 at both the writer and the reader, exactly as
//! the onion and anchor frames are, so a v3-negotiated stream cannot introduce a
//! v4 concept. The relay acknowledges with the ordinary [`Response`] `Ok`
//! ([`write_response`]) rather than a frame of its own — no new codec, and a
//! refused park reports a typed [`RejectReason`] like every other refusal.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::anchor::AccountAnchor;

pub use pollis_device_cert::{MLDSA44_PUB_LEN, MLDSA44_SIG_LEN};
/// The signed-tree-head signature length (ML-DSA-44, 2420 bytes). Named from the
/// log's own constant rather than the device-cert one so the `Anchor` frame is
/// tied to the artifact it actually carries.
pub use verifiable_log::STH_SIG_LEN;

/// An ML-DSA-44 signing key — the device key that proves possession, and the
/// account key that mints certs. Re-exported so callers need not depend on
/// `ml-dsa` directly to name the type.
pub type MlDsaSigningKey = ml_dsa::SigningKey<MlDsa44>;

/// Newest wire protocol version this build speaks. Bumped 3 → 4 for the
/// multi-hop onion protocol (#813): `Extend` and `Layer` are new frames and a
/// relay may now forward to another relay instead of only to a destination.
/// Bumped 2 → 3 for post-quantum authentication (#668).
pub const PROTOCOL_VERSION: u8 = 4;

/// Oldest wire protocol version this build still accepts. v3 stays supported so
/// a v4 client keeps working single-hop against the already-deployed v3 fleet;
/// multi-hop needs v4 at every hop.
pub const MIN_PROTOCOL_VERSION: u8 = 3;

/// ALPN for the outer relay-hop QUIC transport, newest generation. Bumped
/// alongside [`PROTOCOL_VERSION`] so a peer cannot silently half-speak a
/// generation it does not implement.
pub const ALPN: &[u8] = b"pollis-relay/4";

/// The v3 ALPN, still offered so the shipped single-hop fleet stays reachable.
pub const ALPN_V3: &[u8] = b"pollis-relay/3";

/// Every ALPN this build accepts, **newest first** — a rustls server picks the
/// first entry of its own list that the client also offered, so this ordering is
/// what makes two v4 peers negotiate v4 while a v4 peer still meets a v3 one.
pub const SUPPORTED_ALPNS: [&[u8]; 2] = [ALPN, ALPN_V3];

/// The ALPN of the **inner** onion layer (the nested TLS 1.3 session a client
/// runs to each hop past the first). Distinct from the outer QUIC ALPN so a
/// misrouted stream fails negotiation instead of being misparsed.
pub const ONION_ALPN: &[u8] = b"pollis-onion/1";

/// Domain-separation prefix for the handshake canonical message. Deliberately
/// **not** bumped with [`PROTOCOL_VERSION`]: the handshake frame is identical in
/// v3 and v4, and keeping one canonical form is what lets a single client sign
/// once and speak either generation.
pub const HANDSHAKE_DOMAIN: &str = "pollis-relay-v3";

/// Accepted clock skew, in seconds, on either side of the relay clock. Matches
/// the DS replay window (`pollis-delivery` `REPLAY_WINDOW_SECS`).
pub const MAX_SKEW_SECS: i64 = 300;

pub(crate) const MSG_HANDSHAKE: u8 = 1;
pub(crate) const MSG_CONNECT: u8 = 2;
pub(crate) const MSG_EXTEND: u8 = 3;
pub(crate) const MSG_LAYER: u8 = 4;
pub(crate) const MSG_ANCHOR: u8 = 5;
pub(crate) const MSG_PARK: u8 = 6;

/// First version in which [`MSG_EXTEND`] / [`MSG_LAYER`] are legal.
pub const ONION_MIN_VERSION: u8 = 4;

/// First version in which [`MSG_ANCHOR`] is legal. Gated the same way the onion
/// frames are: a v3-negotiated stream must not be able to introduce a v4 concept,
/// in either direction.
pub const ANCHOR_MIN_VERSION: u8 = 4;

/// First version in which [`MSG_PARK`] is legal. Gated the same way the onion
/// and anchor frames are, at the writer and at the reader.
///
/// There is no distinct `ParkAck` frame: a parked connection is acknowledged
/// with the ordinary [`write_response`] `Ok`, which costs no new codec on either
/// side and gives a refusal a typed [`RejectReason`] for free.
pub const PARK_MIN_VERSION: u8 = 4;

/// Cap on the DER leaf a `Park` may carry. A self-signed relay leaf is a few
/// hundred bytes; the bound exists so a peer cannot force an allocation, and it
/// is deliberately separate from [`MAX_STRING_LEN`] rather than a loosening of
/// it.
const MAX_RELAY_LEAF_DER: usize = 8192;

/// Cap on an anchor's audit path. An RFC-6962 path is `ceil(log2(tree_size))`
/// long, so 64 covers a tree with 2^64 leaves — the bound exists to stop a peer
/// forcing an allocation, not to limit the log.
const MAX_AUDIT_PATH: usize = 64;

/// Cap on an anchor's leaf bytes. An account-key leaf is dominated by the hex of
/// the 1312-byte ML-DSA-44 account key (2624 chars), so a real one is ~2.7 KB;
/// [`MAX_STRING_LEN`] is far too small for it and this bound is deliberately
/// separate rather than a loosening of the host/id one.
const MAX_ANCHOR_LEAF: usize = 4096;

const STATUS_OK: u8 = 0;
const STATUS_REJECTED: u8 = 1;

const ADDR_V4: u8 = 4;
const ADDR_V6: u8 = 6;

/// Cap host/id string reads so a peer can't force an unbounded allocation.
const MAX_STRING_LEN: usize = 1024;
const NONCE_LEN: usize = 32;

/// Map an ALPN token (`pollis-relay/N`) to its numeric generation, or `None` if
/// it is not a generation this build supports.
pub fn version_from_alpn(alpn: &[u8]) -> Option<u8> {
    if !SUPPORTED_ALPNS.contains(&alpn) {
        return None;
    }
    let text = std::str::from_utf8(alpn).ok()?;
    let (_, digits) = text.rsplit_once('/')?;
    digits.parse::<u8>().ok()
}

/// True if `version` is a generation this build can parse.
pub fn version_supported(version: u8) -> bool {
    (MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}

/// SHA-256 of a relay's DER leaf cert — the compact pin an `Extend` carries so
/// the forwarding relay refuses to hand the circuit to the wrong next hop.
pub fn cert_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hasher.finalize().into()
}

/// Errors reading or writing the wire protocol.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported protocol version: {0}")]
    BadVersion(u8),
    #[error("unexpected message type: {0}")]
    BadMessageType(u8),
    #[error("malformed frame: {0}")]
    Malformed(&'static str),
    #[error("relay rejected the connection: {0:?}")]
    Rejected(RejectReason),
}

/// Why the relay refused a `Connect`. Each maps to a stable `u8` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Target host is not in the relay's static allowlist (design §1.2).
    NotAllowed,
    /// Missing / forged / expired device signature, or a cert chain that does
    /// not bind the presented device key to the claimed account.
    Unauthorized,
    /// Allowed and authorized, but the relay could not reach the target.
    DialFailed,
    /// Frame was structurally invalid.
    BadRequest,
    /// The client tripped a per-account or per-source-IP rate/concurrency limit
    /// (design §11.5). Returned cleanly rather than dropping the stream.
    RateLimited,
    /// An `Extend` could not be honoured: this relay does not act as a middle
    /// hop, or the next hop was unreachable / presented the wrong pinned cert /
    /// did not speak the onion generation. Deliberately one code — a client
    /// cannot act differently on the distinction, and collapsing them keeps the
    /// relay from reporting anything about its neighbours.
    ExtendFailed,
    /// This node requires a transparency-log account anchor (#813 Phase E2) and
    /// the client presented none. Deliberately distinct from `Unauthorized`: it
    /// is the one rejection the client can *act* on — fetch a fresh inclusion
    /// proof and retry — whereas a presented-but-invalid anchor is an
    /// authentication failure like any forged credential and reports as
    /// `Unauthorized`.
    AnchorRequired,
    /// Relay-side failure.
    Internal,
}

impl RejectReason {
    fn to_code(self) -> u8 {
        match self {
            RejectReason::NotAllowed => 1,
            RejectReason::Unauthorized => 2,
            RejectReason::DialFailed => 3,
            RejectReason::BadRequest => 4,
            RejectReason::RateLimited => 6,
            RejectReason::ExtendFailed => 7,
            RejectReason::AnchorRequired => 8,
            RejectReason::Internal => 5,
        }
    }

    fn from_code(code: u8) -> RejectReason {
        match code {
            1 => RejectReason::NotAllowed,
            2 => RejectReason::Unauthorized,
            3 => RejectReason::DialFailed,
            4 => RejectReason::BadRequest,
            6 => RejectReason::RateLimited,
            7 => RejectReason::ExtendFailed,
            8 => RejectReason::AnchorRequired,
            _ => RejectReason::Internal,
        }
    }
}

/// The device-certificate material a client presents alongside the handshake
/// signature — the offline chain the relay verifies (§9.4). Mirrors the columns
/// a device stores after `ensure_device_cert` (see
/// `pollis-core/src/commands/mls/device.rs`).
#[derive(Debug, Clone)]
pub struct DeviceCertMaterial {
    /// The user's ML-DSA-44 account identity public key ([`MLDSA44_PUB_LEN`]).
    pub account_id_pub: Vec<u8>,
    /// The cert ([`MLDSA44_SIG_LEN`]): the account key's signature binding this
    /// device's two leaf signing keys to `account_id_pub`.
    pub device_cert: Vec<u8>,
    /// Account-key version the cert was minted under.
    pub identity_version: u32,
    /// Unix seconds the cert was issued.
    pub issued_at: u64,
}

impl DeviceCertMaterial {
    /// Mint a cert chain by signing the canonical payload with a known account
    /// key. This is the "minimal seam" (task §2) that lets a holder of the
    /// account signing key — chiefly tests injecting a known account identity —
    /// produce a chain the relay will accept. Production certs are minted by
    /// `pollis-core::commands::account_identity::sign_device_cert`, which signs
    /// the SAME shared payload.
    pub fn mint(
        account: &MlDsaSigningKey,
        device_id: &str,
        device_ed25519_pub: &[u8; 32],
        device_mldsa_pub: &[u8],
        identity_version: u32,
        issued_at: u64,
    ) -> DeviceCertMaterial {
        let payload = pollis_device_cert::device_cert_signed_payload(
            device_id,
            device_ed25519_pub,
            device_mldsa_pub,
            identity_version,
            issued_at,
        )
        .expect("device_id / signing pubs within cert length bounds");
        DeviceCertMaterial {
            account_id_pub: account.verifying_key().encode().to_vec(),
            device_cert: account.sign(&payload).encode().to_vec(),
            identity_version,
            issued_at,
        }
    }
}

/// The device-certificate handshake frame.
#[derive(Debug, Clone)]
pub struct Handshake {
    pub user_id: String,
    pub device_id: String,
    /// The device's classic Ed25519 leaf key, PRESENTED in-band. The cert binds
    /// it, but nothing here verifies under it — it is carried so the cert payload
    /// can be reconstructed byte-for-byte. (In Pollis: `user_device.mls_signature_pub`.)
    pub device_ed25519_pub: [u8; 32],
    /// The device's ML-DSA-44 leaf key, PRESENTED in-band. The handshake
    /// `signature` is verified against it; the `device_cert` chains it to the
    /// account. (In Pollis: `user_device.mls_signature_pub_pq`.)
    pub device_mldsa_pub: Vec<u8>,
    pub cert: DeviceCertMaterial,
    pub timestamp: i64,
    pub nonce: [u8; NONCE_LEN],
    pub signature: Vec<u8>,
}

/// The target-selection frame: an arbitrary `host:port`. The relay — not the
/// protocol — decides whether the host is allowed (design §14.0).
#[derive(Debug, Clone)]
pub struct Connect {
    pub host: String,
    pub port: u16,
}

/// The circuit-extension frame (v4+): "hand this stream to the relay at `addr`".
///
/// `next_cert_sha256` is the SHA-256 of the next hop's DER leaf cert. The
/// forwarding relay pins on it, which keeps `Extend` from being a "dial anything"
/// primitive; the client *independently* pins the full DER inside the onion layer
/// it then runs to that hop, so a forwarding relay that ignored the pin still
/// could not read or substitute the layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extend {
    pub addr: SocketAddr,
    pub next_cert_sha256: [u8; 32],
}

/// The parking frame (v4+): "keep this connection; I serve the relay identity
/// `relay_leaf_der`, and I am reachable only through it."
///
/// It carries the **whole DER**, not the digest, for two reasons: the relay
/// re-checks live revocation on the full identity (the `cert_sha256_b64`
/// selector needs the preimage), and the fingerprint an `Extend` names is then
/// derived here rather than trusted from the peer. The frame is *not* proof that
/// the sender holds the leaf's private key — that is proven later, and
/// unavoidably, by the pinned TLS handshake the relay runs into the parked
/// connection before it splices anything (see [`crate::park`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Park {
    pub relay_leaf_der: Vec<u8>,
}

/// A command a client may send once its handshake has verified.
#[derive(Debug, Clone)]
pub enum Command {
    /// Terminate the circuit here and pipe to a destination (allowlist applies).
    Connect(Connect),
    /// Forward the circuit to another relay (v4+).
    Extend(Extend),
}

/// What a client may legally send after its handshake: at most one optional
/// [`AccountAnchor`], then exactly one [`Command`] — or, for a peer-hosted
/// relay, a single [`Park`] instead of a command.
///
/// Modelled as one enum with one reader so the ordering is enforced by the type
/// the caller matches on, rather than by remembering to peek a header first.
#[derive(Debug, Clone)]
pub enum ClientFrame {
    /// A transparency-log account anchor (v4+, #813 Phase E2).
    Anchor(AccountAnchor),
    /// A peer offering this connection as a middle hop (v4+, #813 Phase P1).
    /// Terminal like a command: nothing follows it on the stream.
    Park(Park),
    /// The single command that ends the negotiation.
    Command(Command),
}

/// The two bytes every frame starts with. Read separately so a relay can
/// dispatch on the message type without committing to a body shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: u8,
    pub msg_type: u8,
}

/// What a verified handshake yields the relay: the authenticated `user_id` plus
/// the account fingerprint the rate limiter keys per-account limits on.
#[derive(Debug, Clone)]
pub struct VerifiedClient {
    pub user_id: String,
    /// `sha256(account_id_pub)`. The rate limiter only ever needs a stable,
    /// collision-resistant handle for "the same account", and the ML-DSA-44
    /// account key is 1312 bytes — hashing keeps the limiter's key a cheap
    /// `Copy` 32 bytes instead of a per-connection heap allocation.
    pub account_fingerprint: [u8; 32],
}

/// Fingerprint an account identity public key for rate-limiting purposes.
pub fn account_fingerprint(account_id_pub: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(account_id_pub);
    hasher.finalize().into()
}

/// Build the canonical signed message. Public so client, relay, and tests all
/// produce byte-for-byte the same bytes.
pub fn handshake_canonical_bytes(user_id: &str, device_id: &str, timestamp: i64, nonce: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    let nonce_hash = hex::encode(hasher.finalize());
    format!("{HANDSHAKE_DOMAIN}\n{user_id}\n{device_id}\n{timestamp}\n{nonce_hash}").into_bytes()
}

/// Produce a signed handshake for `(user_id, device_id)` at `timestamp` with a
/// fresh `nonce`, signed by the device's ML-DSA-44 private key, carrying both
/// presented leaf keys and the device's account cert chain.
pub fn sign_handshake(
    signing_key: &MlDsaSigningKey,
    user_id: &str,
    device_id: &str,
    device_ed25519_pub: [u8; 32],
    cert: DeviceCertMaterial,
    timestamp: i64,
    nonce: [u8; NONCE_LEN],
) -> Handshake {
    let msg = handshake_canonical_bytes(user_id, device_id, timestamp, &nonce);
    Handshake {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        device_ed25519_pub,
        device_mldsa_pub: signing_key.verifying_key().encode().to_vec(),
        cert,
        timestamp,
        nonce,
        signature: signing_key.sign(&msg).encode().to_vec(),
    }
}

/// Verify a handshake with the OFFLINE cert chain (design §9.4, §11.1): bounded
/// timestamp skew, a signature that checks out under the PRESENTED device key,
/// and a device cert that binds that key to the claimed `account_id_pub`. No
/// resolver, no I/O. On success returns the authenticated [`VerifiedClient`].
/// Never fails open — any failure is a [`RejectReason`].
pub fn verify_handshake(hs: &Handshake, now: i64) -> Result<VerifiedClient, RejectReason> {
    if hs.user_id.is_empty() || hs.device_id.is_empty() {
        return Err(RejectReason::Unauthorized);
    }
    if (now - hs.timestamp).abs() > MAX_SKEW_SECS {
        return Err(RejectReason::Unauthorized);
    }

    // (a) Possession: the handshake signature is valid under the presented key.
    let encoded_key: &EncodedVerifyingKey<MlDsa44> = hs
        .device_mldsa_pub
        .as_slice()
        .try_into()
        .map_err(|_| RejectReason::Unauthorized)?;
    let encoded_sig: &EncodedSignature<MlDsa44> = hs
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| RejectReason::Unauthorized)?;
    let device_key = ml_dsa::VerifyingKey::<MlDsa44>::decode(encoded_key);
    let signature =
        ml_dsa::Signature::<MlDsa44>::decode(encoded_sig).ok_or(RejectReason::Unauthorized)?;
    let msg = handshake_canonical_bytes(&hs.user_id, &hs.device_id, hs.timestamp, &hs.nonce);
    device_key
        .verify(&msg, &signature)
        .map_err(|_| RejectReason::Unauthorized)?;

    // (b) Membership: the account key certified this device key (offline chain).
    pollis_device_cert::verify_device_cert(
        &hs.cert.account_id_pub,
        &hs.device_id,
        &hs.device_ed25519_pub,
        &hs.device_mldsa_pub,
        hs.cert.identity_version,
        hs.cert.issued_at,
        &hs.cert.device_cert,
    )
    .map_err(|_| RejectReason::Unauthorized)?;

    Ok(VerifiedClient {
        user_id: hs.user_id.clone(),
        account_fingerprint: account_fingerprint(&hs.cert.account_id_pub),
    })
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---- frame I/O -------------------------------------------------------------

async fn write_string<W: AsyncWrite + Unpin>(w: &mut W, s: &str) -> Result<(), ProtoError> {
    let bytes = s.as_bytes();
    if bytes.len() > MAX_STRING_LEN {
        return Err(ProtoError::Malformed("string too long"));
    }
    w.write_u16(bytes.len() as u16).await?;
    w.write_all(bytes).await?;
    Ok(())
}

async fn read_string<R: AsyncRead + Unpin>(r: &mut R) -> Result<String, ProtoError> {
    let len = r.read_u16().await? as usize;
    if len > MAX_STRING_LEN {
        return Err(ProtoError::Malformed("string too long"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    String::from_utf8(buf).map_err(|_| ProtoError::Malformed("invalid utf-8"))
}

/// Read the two-byte frame header, rejecting any version this build does not
/// speak. Callers dispatch on `msg_type` and then read the matching body.
pub async fn read_frame_header<R: AsyncRead + Unpin>(r: &mut R) -> Result<FrameHeader, ProtoError> {
    let version = r.read_u8().await?;
    if !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    let msg_type = r.read_u8().await?;
    Ok(FrameHeader { version, msg_type })
}

/// Write a handshake frame at wire generation `version`. The frame body is
/// identical in every supported generation; only the version byte differs.
pub async fn write_handshake<W: AsyncWrite + Unpin>(
    w: &mut W,
    hs: &Handshake,
    version: u8,
) -> Result<(), ProtoError> {
    if !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    w.write_u8(version).await?;
    w.write_u8(MSG_HANDSHAKE).await?;
    write_string(w, &hs.user_id).await?;
    write_string(w, &hs.device_id).await?;
    // Every post-quantum field is fixed-width on the wire, so a peer cannot use a
    // length prefix to steer the relay's allocations. Reject wrong-sized material
    // here rather than emitting a frame no v3 reader could parse.
    if hs.device_mldsa_pub.len() != MLDSA44_PUB_LEN
        || hs.cert.account_id_pub.len() != MLDSA44_PUB_LEN
        || hs.cert.device_cert.len() != MLDSA44_SIG_LEN
        || hs.signature.len() != MLDSA44_SIG_LEN
    {
        return Err(ProtoError::Malformed("bad post-quantum field length"));
    }
    w.write_all(&hs.device_ed25519_pub).await?;
    w.write_all(&hs.device_mldsa_pub).await?;
    w.write_all(&hs.cert.account_id_pub).await?;
    w.write_u32(hs.cert.identity_version).await?;
    w.write_u64(hs.cert.issued_at).await?;
    w.write_all(&hs.cert.device_cert).await?;
    w.write_i64(hs.timestamp).await?;
    w.write_u8(NONCE_LEN as u8).await?;
    w.write_all(&hs.nonce).await?;
    w.write_all(&hs.signature).await?;
    w.flush().await?;
    Ok(())
}

/// Read a whole handshake frame (header + body).
pub async fn read_handshake<R: AsyncRead + Unpin>(r: &mut R) -> Result<Handshake, ProtoError> {
    let header = read_frame_header(r).await?;
    if header.msg_type != MSG_HANDSHAKE {
        return Err(ProtoError::BadMessageType(header.msg_type));
    }
    read_handshake_body(r).await
}

/// Read a handshake frame's body, the header having already been consumed.
pub async fn read_handshake_body<R: AsyncRead + Unpin>(r: &mut R) -> Result<Handshake, ProtoError> {
    let user_id = read_string(r).await?;
    let device_id = read_string(r).await?;
    let mut device_ed25519_pub = [0u8; 32];
    r.read_exact(&mut device_ed25519_pub).await?;
    let mut device_mldsa_pub = vec![0u8; MLDSA44_PUB_LEN];
    r.read_exact(&mut device_mldsa_pub).await?;
    let mut account_id_pub = vec![0u8; MLDSA44_PUB_LEN];
    r.read_exact(&mut account_id_pub).await?;
    let identity_version = r.read_u32().await?;
    let issued_at = r.read_u64().await?;
    let mut device_cert = vec![0u8; MLDSA44_SIG_LEN];
    r.read_exact(&mut device_cert).await?;
    let timestamp = r.read_i64().await?;
    let nonce_len = r.read_u8().await? as usize;
    if nonce_len != NONCE_LEN {
        return Err(ProtoError::Malformed("bad nonce length"));
    }
    let mut nonce = [0u8; NONCE_LEN];
    r.read_exact(&mut nonce).await?;
    let mut signature = vec![0u8; MLDSA44_SIG_LEN];
    r.read_exact(&mut signature).await?;
    Ok(Handshake {
        user_id,
        device_id,
        device_ed25519_pub,
        device_mldsa_pub,
        cert: DeviceCertMaterial {
            account_id_pub,
            device_cert,
            identity_version,
            issued_at,
        },
        timestamp,
        nonce,
        signature,
    })
}

/// Write a connect frame at wire generation `version`.
pub async fn write_connect<W: AsyncWrite + Unpin>(
    w: &mut W,
    c: &Connect,
    version: u8,
) -> Result<(), ProtoError> {
    if !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    w.write_u8(version).await?;
    w.write_u8(MSG_CONNECT).await?;
    write_string(w, &c.host).await?;
    w.write_u16(c.port).await?;
    w.flush().await?;
    Ok(())
}

/// Read a whole connect frame (header + body).
pub async fn read_connect<R: AsyncRead + Unpin>(r: &mut R) -> Result<Connect, ProtoError> {
    let header = read_frame_header(r).await?;
    if header.msg_type != MSG_CONNECT {
        return Err(ProtoError::BadMessageType(header.msg_type));
    }
    read_connect_body(r).await
}

/// Read a connect frame's body, the header having already been consumed.
pub async fn read_connect_body<R: AsyncRead + Unpin>(r: &mut R) -> Result<Connect, ProtoError> {
    let host = read_string(r).await?;
    let port = r.read_u16().await?;
    Ok(Connect { host, port })
}

/// Write an extend frame. v4+ only — there is no v3 encoding of this frame, so
/// attempting it on a v3-negotiated connection is a programming error, not a
/// downgrade.
pub async fn write_extend<W: AsyncWrite + Unpin>(
    w: &mut W,
    e: &Extend,
    version: u8,
) -> Result<(), ProtoError> {
    if version < ONION_MIN_VERSION || !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    w.write_u8(version).await?;
    w.write_u8(MSG_EXTEND).await?;
    match e.addr.ip() {
        IpAddr::V4(ip) => {
            w.write_u8(ADDR_V4).await?;
            w.write_all(&ip.octets()).await?;
        }
        IpAddr::V6(ip) => {
            w.write_u8(ADDR_V6).await?;
            w.write_all(&ip.octets()).await?;
        }
    }
    w.write_u16(e.addr.port()).await?;
    w.write_all(&e.next_cert_sha256).await?;
    w.flush().await?;
    Ok(())
}

/// Read an extend frame's body, the header having already been consumed.
pub async fn read_extend_body<R: AsyncRead + Unpin>(r: &mut R) -> Result<Extend, ProtoError> {
    let family = r.read_u8().await?;
    let ip = match family {
        ADDR_V4 => {
            let mut octets = [0u8; 4];
            r.read_exact(&mut octets).await?;
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        ADDR_V6 => {
            let mut octets = [0u8; 16];
            r.read_exact(&mut octets).await?;
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => {
            return Err(ProtoError::Malformed("bad address family"));
        }
    };
    let port = r.read_u16().await?;
    let mut next_cert_sha256 = [0u8; 32];
    r.read_exact(&mut next_cert_sha256).await?;
    Ok(Extend {
        addr: SocketAddr::new(ip, port),
        next_cert_sha256,
    })
}

/// Write the relay→relay layer announcement: "the bytes after this are an onion
/// layer addressed to you; terminate it and serve what's inside." Carries no
/// payload — everything about the circuit past this point is inside the layer,
/// which is precisely what keeps it invisible to the sender.
pub async fn write_layer<W: AsyncWrite + Unpin>(w: &mut W, version: u8) -> Result<(), ProtoError> {
    if version < ONION_MIN_VERSION || !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    w.write_u8(version).await?;
    w.write_u8(MSG_LAYER).await?;
    w.flush().await?;
    Ok(())
}

/// Write an account-anchor frame. v4+ only — like the onion frames, there is no
/// older encoding of it, so emitting one at v3 is a programming error rather
/// than a downgrade.
pub async fn write_anchor<W: AsyncWrite + Unpin>(
    w: &mut W,
    a: &AccountAnchor,
    version: u8,
) -> Result<(), ProtoError> {
    if version < ANCHOR_MIN_VERSION || !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    if a.leaf_bytes.len() > MAX_ANCHOR_LEAF {
        return Err(ProtoError::Malformed("anchor leaf too long"));
    }
    if a.audit_path.len() > MAX_AUDIT_PATH {
        return Err(ProtoError::Malformed("anchor audit path too long"));
    }
    if a.sth_signature.len() != STH_SIG_LEN {
        return Err(ProtoError::Malformed("anchor signature is not ML-DSA-44"));
    }
    w.write_u8(version).await?;
    w.write_u8(MSG_ANCHOR).await?;
    w.write_u16(a.leaf_bytes.len() as u16).await?;
    w.write_all(&a.leaf_bytes).await?;
    w.write_u64(a.leaf_index).await?;
    w.write_u64(a.tree_size).await?;
    w.write_u8(a.audit_path.len() as u8).await?;
    for sibling in &a.audit_path {
        w.write_all(sibling).await?;
    }
    w.write_u64(a.sth_timestamp_ms).await?;
    w.write_all(&a.sth_root).await?;
    w.write_all(&a.sth_signature).await?;
    w.flush().await?;
    Ok(())
}

/// Read an anchor frame's body, the header having already been consumed.
pub async fn read_anchor_body<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<AccountAnchor, ProtoError> {
    let leaf_len = r.read_u16().await? as usize;
    if leaf_len > MAX_ANCHOR_LEAF {
        return Err(ProtoError::Malformed("anchor leaf too long"));
    }
    let mut leaf_bytes = vec![0u8; leaf_len];
    r.read_exact(&mut leaf_bytes).await?;
    let leaf_index = r.read_u64().await?;
    let tree_size = r.read_u64().await?;
    let path_len = r.read_u8().await? as usize;
    if path_len > MAX_AUDIT_PATH {
        return Err(ProtoError::Malformed("anchor audit path too long"));
    }
    let mut audit_path = Vec::with_capacity(path_len);
    for _ in 0..path_len {
        let mut sibling = [0u8; 32];
        r.read_exact(&mut sibling).await?;
        audit_path.push(sibling);
    }
    let sth_timestamp_ms = r.read_u64().await?;
    let mut sth_root = [0u8; 32];
    r.read_exact(&mut sth_root).await?;
    let mut sth_signature = vec![0u8; STH_SIG_LEN];
    r.read_exact(&mut sth_signature).await?;
    Ok(AccountAnchor {
        leaf_bytes,
        leaf_index,
        tree_size,
        audit_path,
        sth_root,
        sth_timestamp_ms,
        sth_signature,
    })
}

/// Write a park frame. v4+ only, like the onion and anchor frames.
pub async fn write_park<W: AsyncWrite + Unpin>(
    w: &mut W,
    p: &Park,
    version: u8,
) -> Result<(), ProtoError> {
    if version < PARK_MIN_VERSION || !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    if p.relay_leaf_der.is_empty() || p.relay_leaf_der.len() > MAX_RELAY_LEAF_DER {
        return Err(ProtoError::Malformed("bad relay leaf length"));
    }
    w.write_u8(version).await?;
    w.write_u8(MSG_PARK).await?;
    w.write_u16(p.relay_leaf_der.len() as u16).await?;
    w.write_all(&p.relay_leaf_der).await?;
    w.flush().await?;
    Ok(())
}

/// Read a park frame's body, the header having already been consumed.
pub async fn read_park_body<R: AsyncRead + Unpin>(r: &mut R) -> Result<Park, ProtoError> {
    let len = r.read_u16().await? as usize;
    if len == 0 || len > MAX_RELAY_LEAF_DER {
        return Err(ProtoError::Malformed("bad relay leaf length"));
    }
    let mut relay_leaf_der = vec![0u8; len];
    r.read_exact(&mut relay_leaf_der).await?;
    Ok(Park { relay_leaf_der })
}

/// Read a command frame (header + body) — what a relay expects once a client's
/// handshake has verified. `Extend` is refused below [`ONION_MIN_VERSION`] so a
/// v3-negotiated stream can never request multi-hop forwarding.
pub async fn read_command<R: AsyncRead + Unpin>(r: &mut R) -> Result<Command, ProtoError> {
    match read_client_frame(r).await? {
        ClientFrame::Command(command) => Ok(command),
        ClientFrame::Anchor(_) => Err(ProtoError::BadMessageType(MSG_ANCHOR)),
        ClientFrame::Park(_) => Err(ProtoError::BadMessageType(MSG_PARK)),
    }
}

/// Read the next post-handshake frame: an optional `Anchor`, then either the
/// terminal `Command` or a terminal `Park`.
///
/// Every v4-only frame type is refused below its minimum version here as well as
/// at the writers, so a v3-negotiated stream can neither request multi-hop
/// forwarding, nor claim a transparency-log anchor, nor park itself as a middle
/// hop.
pub async fn read_client_frame<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<ClientFrame, ProtoError> {
    let header = read_frame_header(r).await?;
    match header.msg_type {
        MSG_CONNECT => Ok(ClientFrame::Command(Command::Connect(
            read_connect_body(r).await?,
        ))),
        MSG_EXTEND if header.version >= ONION_MIN_VERSION => Ok(ClientFrame::Command(
            Command::Extend(read_extend_body(r).await?),
        )),
        MSG_ANCHOR if header.version >= ANCHOR_MIN_VERSION => {
            Ok(ClientFrame::Anchor(read_anchor_body(r).await?))
        }
        MSG_PARK if header.version >= PARK_MIN_VERSION => {
            Ok(ClientFrame::Park(read_park_body(r).await?))
        }
        other => Err(ProtoError::BadMessageType(other)),
    }
}

/// Write the terminal response frame at wire generation `version`.
pub async fn write_response<W: AsyncWrite + Unpin>(
    w: &mut W,
    result: Result<(), RejectReason>,
    version: u8,
) -> Result<(), ProtoError> {
    // A response to a peer whose generation we could not parse still has to go
    // out; fall back to the oldest supported generation rather than emitting a
    // version byte nobody can read.
    let version = if version_supported(version) {
        version
    } else {
        MIN_PROTOCOL_VERSION
    };
    w.write_u8(version).await?;
    match result {
        Ok(()) => {
            w.write_u8(STATUS_OK).await?;
        }
        Err(reason) => {
            w.write_u8(STATUS_REJECTED).await?;
            w.write_u8(reason.to_code()).await?;
            write_string(w, "").await?;
        }
    }
    w.flush().await?;
    Ok(())
}

/// Read the terminal response frame. `Ok(())` means the byte pipe is live.
pub async fn read_response<R: AsyncRead + Unpin>(r: &mut R) -> Result<(), ProtoError> {
    let version = r.read_u8().await?;
    if !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    let status = r.read_u8().await?;
    match status {
        STATUS_OK => Ok(()),
        STATUS_REJECTED => {
            let code = r.read_u8().await?;
            let _detail = read_string(r).await?;
            Err(ProtoError::Rejected(RejectReason::from_code(code)))
        }
        _ => Err(ProtoError::Malformed("bad status")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic ML-DSA-44 key from a one-byte seed pattern, so every test
    /// key is reproducible and distinct.
    fn key(seed_byte: u8) -> MlDsaSigningKey {
        MlDsaSigningKey::from_seed(&[seed_byte; 32].into())
    }

    fn pq_pub(k: &MlDsaSigningKey) -> Vec<u8> {
        k.verifying_key().encode().to_vec()
    }

    /// The classic leaf key is only carried, never verified under, so tests can
    /// stand in an arbitrary 32-byte value for it.
    const ED_PUB: [u8; 32] = [3u8; 32];

    #[test]
    fn valid_chain_accepts() {
        let account = key(5);
        let device = key(6);
        let cert =
            DeviceCertMaterial::mint(&account, "d1", &ED_PUB, &pq_pub(&device), 1, 1_700_000_000);
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);
        let v = verify_handshake(&hs, now).expect("valid chain");
        assert_eq!(v.user_id, "u1");
        assert_eq!(
            v.account_fingerprint,
            account_fingerprint(&pq_pub(&account))
        );
    }

    #[test]
    fn forged_cert_rejected() {
        // Device key NOT certified by the presented account: the attacker signs a
        // cert with the WRONG (their own) account key but claims the victim's
        // account_id_pub.
        let victim_account = key(5);
        let attacker_account = key(9);
        let device = key(6);
        let mut cert =
            DeviceCertMaterial::mint(&attacker_account, "d1", &ED_PUB, &pq_pub(&device), 1, 1);
        // Claim the victim's account id, but the signature is the attacker's.
        cert.account_id_pub = pq_pub(&victim_account);
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn cert_for_other_device_key_rejected() {
        // A valid cert, but the handshake is signed by a DIFFERENT device key than
        // the cert binds — possession and membership refer to different keys.
        let account = key(5);
        let real_device = key(6);
        let other_device = key(7);
        let cert = DeviceCertMaterial::mint(&account, "d1", &ED_PUB, &pq_pub(&real_device), 1, 1);
        let now = now_unix();
        // Sign with other_device; its pub is presented, but the cert binds real_device.
        let hs = sign_handshake(&other_device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn swapped_classic_leaf_key_rejected() {
        // The classic leaf key is bound by the cert too (cert v2, #668), so
        // presenting a different one breaks membership even though possession of
        // the ML-DSA key is genuine.
        let account = key(5);
        let device = key(6);
        let cert = DeviceCertMaterial::mint(&account, "d1", &ED_PUB, &pq_pub(&device), 1, 1);
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", [4u8; 32], cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn expired_and_forged_signature_rejected() {
        let account = key(5);
        let device = key(6);
        let cert = DeviceCertMaterial::mint(&account, "d1", &ED_PUB, &pq_pub(&device), 1, 1);
        let now = now_unix();

        // Expired.
        let expired =
            sign_handshake(&device, "u1", "d1", ED_PUB, cert.clone(), now - 10_000, [1u8; 32]);
        assert_eq!(verify_handshake(&expired, now).unwrap_err(), RejectReason::Unauthorized);

        // Forged signature byte.
        let mut forged = sign_handshake(&device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);
        forged.signature[0] ^= 0xFF;
        assert_eq!(verify_handshake(&forged, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[tokio::test]
    async fn park_frames_roundtrip() {
        let park = Park {
            relay_leaf_der: vec![0xAB; 300],
        };
        let mut buf = Vec::new();
        write_park(&mut buf, &park, PROTOCOL_VERSION).await.expect("write");
        let mut r = buf.as_slice();
        match read_client_frame(&mut r).await.expect("read") {
            ClientFrame::Park(read) => assert_eq!(read, park),
            other => panic!("expected a park frame, got {other:?}"),
        }

        // The acknowledgement is the ordinary Ok response — the device side
        // reads it with `read_response`, so there is no second codec.
        let mut ack = Vec::new();
        write_response(&mut ack, Ok(()), PROTOCOL_VERSION).await.expect("write ack");
        read_response(&mut ack.as_slice()).await.expect("read ack");
    }

    /// A refused park reports its reason through that same response, so a peer
    /// learns *why* instead of seeing a stream close.
    #[tokio::test]
    async fn a_refused_park_decodes_as_its_reason() {
        let mut buf = Vec::new();
        write_response(&mut buf, Err(RejectReason::RateLimited), PROTOCOL_VERSION)
            .await
            .expect("write");
        let err = read_response(&mut buf.as_slice()).await.unwrap_err();
        assert!(matches!(
            err,
            ProtoError::Rejected(RejectReason::RateLimited)
        ));
    }

    /// The v3 gate, at the writer. Same shape as the `Extend` / `Anchor` gates:
    /// there is no v3 encoding of this frame, so emitting one is a programming
    /// error rather than a downgrade.
    #[tokio::test]
    async fn park_cannot_be_written_at_v3() {
        let park = Park {
            relay_leaf_der: vec![1u8; 32],
        };
        let mut buf = Vec::new();
        assert!(matches!(
            write_park(&mut buf, &park, MIN_PROTOCOL_VERSION).await,
            Err(ProtoError::BadVersion(3))
        ));
        assert!(buf.is_empty(), "nothing may be emitted at v3");
    }

    /// The v3 gate, at the reader — the one that actually protects the relay,
    /// because a hostile peer does not use our writer. A hand-rolled v3-versioned
    /// park frame is refused as an unknown message type.
    #[tokio::test]
    async fn a_v3_stream_cannot_smuggle_a_park() {
        let mut raw = vec![MIN_PROTOCOL_VERSION, MSG_PARK, 0, 4, 1, 2, 3, 4];
        let err = read_client_frame(&mut raw.as_slice()).await.unwrap_err();
        assert!(matches!(err, ProtoError::BadMessageType(MSG_PARK)));

        // …and the same bytes at v4 parse, so the test is pinning the VERSION
        // gate and not a typo in the frame body.
        raw[0] = PROTOCOL_VERSION;
        assert!(matches!(
            read_client_frame(&mut raw.as_slice()).await,
            Ok(ClientFrame::Park(_))
        ));
    }

    /// A park is not a command: `read_command` refuses it rather than silently
    /// treating it as one.
    #[tokio::test]
    async fn read_command_refuses_a_park() {
        let park = Park {
            relay_leaf_der: vec![7u8; 16],
        };
        let mut buf = Vec::new();
        write_park(&mut buf, &park, PROTOCOL_VERSION).await.expect("write");
        assert!(matches!(
            read_command(&mut buf.as_slice()).await,
            Err(ProtoError::BadMessageType(MSG_PARK))
        ));
    }

    /// An empty or oversized leaf is refused at both ends, so neither can force
    /// an allocation or park under a degenerate identity.
    #[tokio::test]
    async fn park_leaf_bounds_are_enforced() {
        let mut buf = Vec::new();
        assert!(write_park(
            &mut buf,
            &Park {
                relay_leaf_der: Vec::new()
            },
            PROTOCOL_VERSION
        )
        .await
        .is_err());
        assert!(write_park(
            &mut buf,
            &Park {
                relay_leaf_der: vec![0u8; MAX_RELAY_LEAF_DER + 1]
            },
            PROTOCOL_VERSION
        )
        .await
        .is_err());

        let empty = [PROTOCOL_VERSION, MSG_PARK, 0, 0];
        assert!(read_client_frame(&mut empty.as_slice()).await.is_err());
    }

    #[tokio::test]
    async fn frame_roundtrips() {
        let account = key(5);
        let device = key(6);
        let cert = DeviceCertMaterial::mint(&account, "d1", &ED_PUB, &pq_pub(&device), 2, 99);
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);

        let mut buf = Vec::new();
        write_handshake(&mut buf, &hs, PROTOCOL_VERSION).await.expect("write");
        let read = read_handshake(&mut buf.as_slice()).await.expect("read");

        assert_eq!(read.user_id, hs.user_id);
        assert_eq!(read.device_ed25519_pub, hs.device_ed25519_pub);
        assert_eq!(read.device_mldsa_pub, hs.device_mldsa_pub);
        assert_eq!(read.cert.account_id_pub, hs.cert.account_id_pub);
        assert_eq!(read.cert.device_cert, hs.cert.device_cert);
        assert_eq!(read.signature, hs.signature);
        verify_handshake(&read, now).expect("roundtripped frame still verifies");
    }
}
