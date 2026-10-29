//! The shared relay wire protocol + the device-certificate handshake. Design:
//! `docs/relay-overlay-design.md`.
//!
//! # Framing
//!
//! Integers are big-endian; strings are UTF-8 with a `u16` length prefix. The
//! client opens one QUIC bi-stream per target and speaks, in order:
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
//! Single-hop: the client pipelines Handshake + Connect; the relay sends exactly
//! one Response, closes on rejection, and after `Ok` the stream is the raw pipe.
//!
//! # Onion layering (v4, design §6.2)
//!
//! Multi-hop circuits telescope: hop *i* gets Handshake + `Extend(hop i+1)`,
//! dials it, announces with `Layer`, splices, answers `Ok`; the client then nests
//! one TLS 1.3 session per hop ([`crate::onion`]) pinning each hop's leaf. The
//! property is network-address unlinkability, not anonymity: the last hop still
//! authenticates the device cert.
//!
//! # Versioning
//!
//! The ALPN carries the generation and a relay offers every supported ALPN
//! ([`SUPPORTED_ALPNS`], newest first), so a v4 client still reaches the v3 fleet
//! single-hop and a mismatched peer fails at ALPN, never mid-stream. The v4-only
//! frames are also gated on the version byte at both writer and reader. The
//! Handshake frame is byte-identical in v3 and v4, hence the unchanged
//! [`HANDSHAKE_DOMAIN`].
//!
//! # Handshake auth: offline device-certificate chain (design §9.4, §11.1)
//!
//! The relay verifies the presented chain with zero I/O, which keeps the relay
//! tier out of the metadata plane. Both checks must pass:
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
//! Every signature is ML-DSA-44 (#668); the Ed25519 leaf is carried and bound by
//! the cert but nothing verifies under it. With first-party-only destinations
//! (§1.2), "a well-formed device, rate-limited" is the anti-abuse bar.
//!
//! # Account anchoring (v4+, optional, #813)
//!
//! Self-consistency is not membership: an attacker can mint account keys and
//! certs offline in a loop. `Anchor` carries an inclusion proof into the
//! account-key transparency log, verified offline ([`crate::anchor`]); whether
//! it is required is node policy ([`crate::anchor::AnchorPolicy`]).
//!
//! # Parking: the reverse hop (v4+, #813)
//!
//! A peer-hosted relay behind NAT can only be reached over a connection it opens
//! outbound. After the ordinary device handshake it sends `Park` naming its relay
//! leaf; the relay keeps the connection keyed on `sha256(relay_leaf_der)` and
//! splices a later matching `Extend` into it ([`crate::park`]). It is
//! acknowledged with the ordinary `Ok` response.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::anchor::AccountAnchor;

pub use pollis_device_cert::{MLDSA44_PUB_LEN, MLDSA44_SIG_LEN};
pub use verifiable_log::STH_SIG_LEN;

/// An ML-DSA-44 signing key (device or account key).
pub type MlDsaSigningKey = ml_dsa::SigningKey<MlDsa44>;

/// Newest wire protocol version this build speaks (4: multi-hop, #813).
pub const PROTOCOL_VERSION: u8 = 4;

/// Oldest accepted version; v3 keeps single-hop working against the v3 fleet.
pub const MIN_PROTOCOL_VERSION: u8 = 3;

/// ALPN for the outer relay-hop QUIC transport; bumped with [`PROTOCOL_VERSION`].
pub const ALPN: &[u8] = b"pollis-relay/4";

/// The v3 ALPN, still offered so the shipped single-hop fleet stays reachable.
pub const ALPN_V3: &[u8] = b"pollis-relay/3";

/// Every ALPN this build accepts, newest first: rustls servers pick by their own
/// order, so this makes v4 peers negotiate v4.
pub const SUPPORTED_ALPNS: [&[u8]; 2] = [ALPN, ALPN_V3];

/// ALPN of the inner onion TLS layer; distinct from the outer one so a misrouted
/// stream fails negotiation instead of being misparsed.
pub const ONION_ALPN: &[u8] = b"pollis-onion/1";

/// Domain-separation prefix for the handshake canonical message. Deliberately
/// not bumped with [`PROTOCOL_VERSION`]: the handshake is identical in v3 and v4.
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

/// First version in which [`MSG_ANCHOR`] is legal.
pub const ANCHOR_MIN_VERSION: u8 = 4;

/// First version in which [`MSG_PARK`] is legal. A park is acknowledged with the
/// ordinary [`write_response`] `Ok`; there is no `ParkAck` frame.
pub const PARK_MIN_VERSION: u8 = 4;

/// Allocation bound on a `Park` leaf (a real relay leaf is a few hundred bytes).
const MAX_RELAY_LEAF_DER: usize = 8192;

/// Allocation bound on an anchor audit path (64 covers a 2^64-leaf tree).
const MAX_AUDIT_PATH: usize = 64;

/// Allocation bound on an anchor leaf (a real one is ~2.7 KB, dominated by the
/// hex ML-DSA-44 account key).
const MAX_ANCHOR_LEAF: usize = 4096;

const STATUS_OK: u8 = 0;
const STATUS_REJECTED: u8 = 1;

const ADDR_V4: u8 = 4;
const ADDR_V6: u8 = 6;

/// Cap host/id string reads so a peer can't force an unbounded allocation.
const MAX_STRING_LEN: usize = 1024;
const NONCE_LEN: usize = 32;

/// Map a supported ALPN token (`pollis-relay/N`) to its generation.
pub fn version_from_alpn(alpn: &[u8]) -> Option<u8> {
    if !SUPPORTED_ALPNS.contains(&alpn) {
        return None;
    }
    let text = std::str::from_utf8(alpn).ok()?;
    let (_, digits) = text.rsplit_once('/')?;
    digits.parse::<u8>().ok()
}

pub fn version_supported(version: u8) -> bool {
    (MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}

/// SHA-256 of a relay's DER leaf cert: the next-hop pin an `Extend` carries.
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
    /// A per-account or per-source-IP rate/concurrency limit (design §11.5).
    RateLimited,
    /// Any `Extend` failure. Deliberately one code: the client can't act on the
    /// distinction, and it keeps the relay from reporting on its neighbours.
    ExtendFailed,
    /// An anchor is required and none was presented: the client can fetch a
    /// proof and retry. An invalid anchor reports as `Unauthorized`.
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

/// The offline device-cert chain a client presents with its handshake (§9.4).
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
    /// Mint a cert chain with a known account key (mainly tests). Production
    /// certs come from `pollis-core`'s `sign_device_cert` over the same payload.
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

#[derive(Debug, Clone)]
pub struct Handshake {
    pub user_id: String,
    pub device_id: String,
    /// Classic Ed25519 leaf key; carried only so the cert payload can be rebuilt.
    pub device_ed25519_pub: [u8; 32],
    /// ML-DSA-44 leaf key the handshake `signature` verifies under.
    pub device_mldsa_pub: Vec<u8>,
    pub cert: DeviceCertMaterial,
    pub timestamp: i64,
    pub nonce: [u8; NONCE_LEN],
    pub signature: Vec<u8>,
}

/// The target-selection frame; the relay, not the protocol, decides whether the
/// host is allowed (design §14.0).
#[derive(Debug, Clone)]
pub struct Connect {
    pub host: String,
    pub port: u16,
}

/// The circuit-extension frame (v4+). The forwarding relay pins the next hop on
/// `next_cert_sha256` so `Extend` isn't a "dial anything"; the client also pins
/// the full DER inside its onion layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extend {
    pub addr: SocketAddr,
    pub next_cert_sha256: [u8; 32],
}

/// The parking frame (v4+). It carries the whole DER so the relay can check
/// revocation and derive the fingerprint itself. It does not prove key
/// possession; the pinned TLS handshake before splicing does ([`crate::park`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Park {
    pub relay_leaf_der: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Terminate the circuit here and pipe to a destination (allowlist applies).
    Connect(Connect),
    /// Forward the circuit to another relay (v4+).
    Extend(Extend),
}

/// What a client may send after its handshake: at most one [`AccountAnchor`],
/// then exactly one [`Command`] or a terminal [`Park`].
#[derive(Debug, Clone)]
pub enum ClientFrame {
    Anchor(AccountAnchor),
    Park(Park),
    Command(Command),
}

/// The two bytes every frame starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: u8,
    pub msg_type: u8,
}

/// What a verified handshake yields the relay.
#[derive(Debug, Clone)]
pub struct VerifiedClient {
    pub user_id: String,
    /// `sha256(account_id_pub)`: a cheap per-account rate-limit key.
    pub account_fingerprint: [u8; 32],
}

pub fn account_fingerprint(account_id_pub: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(account_id_pub);
    hasher.finalize().into()
}

/// The canonical signed handshake message.
pub fn handshake_canonical_bytes(user_id: &str, device_id: &str, timestamp: i64, nonce: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    let nonce_hash = hex::encode(hasher.finalize());
    format!("{HANDSHAKE_DOMAIN}\n{user_id}\n{device_id}\n{timestamp}\n{nonce_hash}").into_bytes()
}

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

/// Verify a handshake offline (design §9.4): bounded skew, possession of the
/// presented device key, and a cert binding it to `account_id_pub`.
pub fn verify_handshake(hs: &Handshake, now: i64) -> Result<VerifiedClient, RejectReason> {
    if hs.user_id.is_empty() || hs.device_id.is_empty() {
        return Err(RejectReason::Unauthorized);
    }
    if (now - hs.timestamp).abs() > MAX_SKEW_SECS {
        return Err(RejectReason::Unauthorized);
    }

    // Possession.
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

    // Membership.
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

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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

/// Read the two-byte frame header, rejecting unsupported versions.
pub async fn read_frame_header<R: AsyncRead + Unpin>(r: &mut R) -> Result<FrameHeader, ProtoError> {
    let version = r.read_u8().await?;
    if !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    let msg_type = r.read_u8().await?;
    Ok(FrameHeader { version, msg_type })
}

/// Version gate every frame writer applies before emitting a byte. Check-only so
/// a refused frame leaves the stream untouched.
fn check_version(version: u8, min: u8) -> Result<(), ProtoError> {
    if version < min || !version_supported(version) {
        return Err(ProtoError::BadVersion(version));
    }
    Ok(())
}

/// Write a handshake frame; only the version byte differs across generations.
pub async fn write_handshake<W: AsyncWrite + Unpin>(
    w: &mut W,
    hs: &Handshake,
    version: u8,
) -> Result<(), ProtoError> {
    check_version(version, MIN_PROTOCOL_VERSION)?;
    w.write_u8(version).await?;
    w.write_u8(MSG_HANDSHAKE).await?;
    write_string(w, &hs.user_id).await?;
    write_string(w, &hs.device_id).await?;
    // PQ fields are fixed-width on the wire (no length prefix to steer
    // allocations), so wrong-sized material is refused here.
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

pub async fn read_handshake<R: AsyncRead + Unpin>(r: &mut R) -> Result<Handshake, ProtoError> {
    let header = read_frame_header(r).await?;
    if header.msg_type != MSG_HANDSHAKE {
        return Err(ProtoError::BadMessageType(header.msg_type));
    }
    read_handshake_body(r).await
}

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

pub async fn write_connect<W: AsyncWrite + Unpin>(
    w: &mut W,
    c: &Connect,
    version: u8,
) -> Result<(), ProtoError> {
    check_version(version, MIN_PROTOCOL_VERSION)?;
    w.write_u8(version).await?;
    w.write_u8(MSG_CONNECT).await?;
    write_string(w, &c.host).await?;
    w.write_u16(c.port).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_connect_body<R: AsyncRead + Unpin>(r: &mut R) -> Result<Connect, ProtoError> {
    let host = read_string(r).await?;
    let port = r.read_u16().await?;
    Ok(Connect { host, port })
}

/// Write an extend frame (v4+ only).
pub async fn write_extend<W: AsyncWrite + Unpin>(
    w: &mut W,
    e: &Extend,
    version: u8,
) -> Result<(), ProtoError> {
    check_version(version, ONION_MIN_VERSION)?;
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

/// Write the relay→relay layer announcement: what follows is an onion layer for
/// the receiver to terminate. No payload, so nothing leaks to the sender.
pub async fn write_layer<W: AsyncWrite + Unpin>(w: &mut W, version: u8) -> Result<(), ProtoError> {
    check_version(version, ONION_MIN_VERSION)?;
    w.write_u8(version).await?;
    w.write_u8(MSG_LAYER).await?;
    w.flush().await?;
    Ok(())
}

/// Write an account-anchor frame (v4+ only).
pub async fn write_anchor<W: AsyncWrite + Unpin>(
    w: &mut W,
    a: &AccountAnchor,
    version: u8,
) -> Result<(), ProtoError> {
    check_version(version, ANCHOR_MIN_VERSION)?;
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

/// Write a park frame (v4+ only).
pub async fn write_park<W: AsyncWrite + Unpin>(
    w: &mut W,
    p: &Park,
    version: u8,
) -> Result<(), ProtoError> {
    check_version(version, PARK_MIN_VERSION)?;
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

pub async fn read_park_body<R: AsyncRead + Unpin>(r: &mut R) -> Result<Park, ProtoError> {
    let len = r.read_u16().await? as usize;
    if len == 0 || len > MAX_RELAY_LEAF_DER {
        return Err(ProtoError::Malformed("bad relay leaf length"));
    }
    let mut relay_leaf_der = vec![0u8; len];
    r.read_exact(&mut relay_leaf_der).await?;
    Ok(Park { relay_leaf_der })
}

/// Read a command frame (header + body); anchors and parks are refused.
pub async fn read_command<R: AsyncRead + Unpin>(r: &mut R) -> Result<Command, ProtoError> {
    match read_client_frame(r).await? {
        ClientFrame::Command(command) => Ok(command),
        ClientFrame::Anchor(_) => Err(ProtoError::BadMessageType(MSG_ANCHOR)),
        ClientFrame::Park(_) => Err(ProtoError::BadMessageType(MSG_PARK)),
    }
}

/// Read the next post-handshake frame. v4-only frames are refused below their
/// minimum version here too, since a hostile peer doesn't use our writers.
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
    // A peer whose version we couldn't parse still gets a readable response.
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

/// Read the terminal response frame; `Ok(())` means the byte pipe is live.
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

    fn key(seed_byte: u8) -> MlDsaSigningKey {
        MlDsaSigningKey::from_seed(&[seed_byte; 32].into())
    }

    fn pq_pub(k: &MlDsaSigningKey) -> Vec<u8> {
        k.verifying_key().encode().to_vec()
    }

    /// Only carried, never verified under, so any value works.
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
        // Attacker signs with their own account key but claims the victim's.
        let victim_account = key(5);
        let attacker_account = key(9);
        let device = key(6);
        let mut cert =
            DeviceCertMaterial::mint(&attacker_account, "d1", &ED_PUB, &pq_pub(&device), 1, 1);
        cert.account_id_pub = pq_pub(&victim_account);
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn cert_for_other_device_key_rejected() {
        // Handshake signed by a different device key than the cert binds.
        let account = key(5);
        let real_device = key(6);
        let other_device = key(7);
        let cert = DeviceCertMaterial::mint(&account, "d1", &ED_PUB, &pq_pub(&real_device), 1, 1);
        let now = now_unix();
        let hs = sign_handshake(&other_device, "u1", "d1", ED_PUB, cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn swapped_classic_leaf_key_rejected() {
        // The cert binds the classic leaf key too (#668).
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

        let expired =
            sign_handshake(&device, "u1", "d1", ED_PUB, cert.clone(), now - 10_000, [1u8; 32]);
        assert_eq!(verify_handshake(&expired, now).unwrap_err(), RejectReason::Unauthorized);

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

        let mut ack = Vec::new();
        write_response(&mut ack, Ok(()), PROTOCOL_VERSION).await.expect("write ack");
        read_response(&mut ack.as_slice()).await.expect("read ack");
    }

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

    /// The reader gate is the one that protects the relay: a hostile peer doesn't
    /// use our writer.
    #[tokio::test]
    async fn a_v3_stream_cannot_smuggle_a_park() {
        let mut raw = vec![MIN_PROTOCOL_VERSION, MSG_PARK, 0, 4, 1, 2, 3, 4];
        let err = read_client_frame(&mut raw.as_slice()).await.unwrap_err();
        assert!(matches!(err, ProtoError::BadMessageType(MSG_PARK)));

        // The same bytes at v4 parse, so this pins the version gate.
        raw[0] = PROTOCOL_VERSION;
        assert!(matches!(
            read_client_frame(&mut raw.as_slice()).await,
            Ok(ClientFrame::Park(_))
        ));
    }

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
