//! The shared relay wire protocol + the device-signature handshake.
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
//! │ u8   version            (== PROTOCOL_VERSION)              │
//! │ u8   msg_type           (== MSG_HANDSHAKE)                 │
//! │ u16  user_id_len | user_id bytes            (UTF-8)        │
//! │ u16  device_id_len | device_id bytes        (UTF-8)       │
//! │ i64  timestamp          (unix seconds, big-endian)        │
//! │ u8   nonce_len (== 32) | nonce bytes                      │
//! │ [64] signature          (Ed25519, over the canonical msg) │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────────── Connect (client → relay) ──────────────────┐
//! │ u8   version            (== PROTOCOL_VERSION)              │
//! │ u8   msg_type           (== MSG_CONNECT)                  │
//! │ u16  host_len | host bytes                  (UTF-8)       │
//! │ u16  port                                                 │
//! └────────────────────────────────────────────────────────────┘
//! ┌──────────────── Response (relay → client) ─────────────────┐
//! │ u8   version            (== PROTOCOL_VERSION)              │
//! │ u8   status             (0 = Ok, 1 = Rejected)            │
//! │ -- if Rejected: --                                        │
//! │ u8   reason_code                                          │
//! │ u16  detail_len | detail bytes              (UTF-8)       │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! The client pipelines Handshake + Connect without waiting; the relay reads
//! both, then sends exactly one Response communicating the final outcome. On any
//! rejection the relay closes the stream. After an `Ok`, the stream becomes the
//! raw byte pipe to the target.
//!
//! # Handshake auth
//!
//! Mirrors `pollis-delivery/src/auth.rs`: the client signs with its Ed25519
//! device private key and the relay verifies against the registered public key.
//! The canonical signed message (UTF-8, `\n` = 0x0A, no trailing newline) is the
//! METHOD-less relay variant:
//!
//! ```text
//! pollis-relay-v1\n{user_id}\n{device_id}\n{timestamp}\n{sha256_hex(nonce)}
//! ```
//!
//! For this slice the relay accepts any well-formed device signature whose
//! self-consistency verifies against the public key supplied by a [`KeyResolver`]
//! (an in-memory resolver backs the tests; a Turso-backed one is a later slice).

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wire protocol version. Bumped only on an incompatible framing change.
pub const PROTOCOL_VERSION: u8 = 1;

/// ALPN for the outer relay-hop QUIC transport.
pub const ALPN: &[u8] = b"pollis-relay/1";

/// Domain-separation prefix for the handshake canonical message.
pub const HANDSHAKE_DOMAIN: &str = "pollis-relay-v1";

/// Accepted clock skew, in seconds, on either side of the relay clock. Matches
/// the DS replay window (`pollis-delivery` `REPLAY_WINDOW_SECS`).
pub const MAX_SKEW_SECS: i64 = 300;

const MSG_HANDSHAKE: u8 = 1;
const MSG_CONNECT: u8 = 2;

const STATUS_OK: u8 = 0;
const STATUS_REJECTED: u8 = 1;

/// Cap host/id string reads so a peer can't force an unbounded allocation.
const MAX_STRING_LEN: usize = 1024;
const NONCE_LEN: usize = 32;

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
    /// Missing / forged / expired device signature.
    Unauthorized,
    /// Allowed and authorized, but the relay could not reach the target.
    DialFailed,
    /// Frame was structurally invalid.
    BadRequest,
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
            RejectReason::Internal => 5,
        }
    }

    fn from_code(code: u8) -> RejectReason {
        match code {
            1 => RejectReason::NotAllowed,
            2 => RejectReason::Unauthorized,
            3 => RejectReason::DialFailed,
            4 => RejectReason::BadRequest,
            _ => RejectReason::Internal,
        }
    }
}

/// The device-signature handshake frame.
#[derive(Debug, Clone)]
pub struct Handshake {
    pub user_id: String,
    pub device_id: String,
    pub timestamp: i64,
    pub nonce: [u8; NONCE_LEN],
    pub signature: [u8; 64],
}

/// The target-selection frame: an arbitrary `host:port`. The relay — not the
/// protocol — decides whether the host is allowed (design §14.0).
#[derive(Debug, Clone)]
pub struct Connect {
    pub host: String,
    pub port: u16,
}

/// Look up the registered Ed25519 public key for a device. A Turso-backed
/// resolver is a later slice; [`InMemoryKeyResolver`] backs the tests.
pub trait KeyResolver: Send + Sync {
    fn resolve(&self, user_id: &str, device_id: &str) -> Option<VerifyingKey>;
}

/// In-memory `(user_id, device_id) → VerifyingKey` map.
#[derive(Debug, Default, Clone)]
pub struct InMemoryKeyResolver {
    keys: std::collections::HashMap<(String, String), VerifyingKey>,
}

impl InMemoryKeyResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, user_id: impl Into<String>, device_id: impl Into<String>, key: VerifyingKey) {
        self.keys.insert((user_id.into(), device_id.into()), key);
    }
}

impl KeyResolver for InMemoryKeyResolver {
    fn resolve(&self, user_id: &str, device_id: &str) -> Option<VerifyingKey> {
        self.keys.get(&(user_id.to_string(), device_id.to_string())).copied()
    }
}

/// Lowercase hex, no separators (avoids a `hex` dependency for one digest).
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Build the canonical signed message. Public so client, relay, and tests all
/// produce byte-for-byte the same bytes.
pub fn handshake_canonical_bytes(user_id: &str, device_id: &str, timestamp: i64, nonce: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    let nonce_hash = hex_lower(&hasher.finalize());
    format!("{HANDSHAKE_DOMAIN}\n{user_id}\n{device_id}\n{timestamp}\n{nonce_hash}").into_bytes()
}

/// Produce a signed handshake for `(user_id, device_id)` at `timestamp` with a
/// fresh `nonce`, signed by the device's Ed25519 private key.
pub fn sign_handshake(
    signing_key: &SigningKey,
    user_id: &str,
    device_id: &str,
    timestamp: i64,
    nonce: [u8; NONCE_LEN],
) -> Handshake {
    let msg = handshake_canonical_bytes(user_id, device_id, timestamp, &nonce);
    let signature: Signature = signing_key.sign(&msg);
    Handshake {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        timestamp,
        nonce,
        signature: signature.to_bytes(),
    }
}

/// Verify a handshake: bounded timestamp skew, resolvable device key, and a
/// signature that checks out over the canonical message. On success returns the
/// authenticated `user_id`. Never fails open — any failure is a [`RejectReason`].
pub fn verify_handshake(
    resolver: &dyn KeyResolver,
    hs: &Handshake,
    now: i64,
) -> Result<String, RejectReason> {
    if hs.user_id.is_empty() || hs.device_id.is_empty() {
        return Err(RejectReason::Unauthorized);
    }
    if (now - hs.timestamp).abs() > MAX_SKEW_SECS {
        return Err(RejectReason::Unauthorized);
    }
    let verifying_key = resolver
        .resolve(&hs.user_id, &hs.device_id)
        .ok_or(RejectReason::Unauthorized)?;
    let msg = handshake_canonical_bytes(&hs.user_id, &hs.device_id, hs.timestamp, &hs.nonce);
    let signature = Signature::from_bytes(&hs.signature);
    verifying_key
        .verify(&msg, &signature)
        .map_err(|_| RejectReason::Unauthorized)?;
    Ok(hs.user_id.clone())
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

/// Write a handshake frame.
pub async fn write_handshake<W: AsyncWrite + Unpin>(w: &mut W, hs: &Handshake) -> Result<(), ProtoError> {
    w.write_u8(PROTOCOL_VERSION).await?;
    w.write_u8(MSG_HANDSHAKE).await?;
    write_string(w, &hs.user_id).await?;
    write_string(w, &hs.device_id).await?;
    w.write_i64(hs.timestamp).await?;
    w.write_u8(NONCE_LEN as u8).await?;
    w.write_all(&hs.nonce).await?;
    w.write_all(&hs.signature).await?;
    w.flush().await?;
    Ok(())
}

/// Read a handshake frame.
pub async fn read_handshake<R: AsyncRead + Unpin>(r: &mut R) -> Result<Handshake, ProtoError> {
    let version = r.read_u8().await?;
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::BadVersion(version));
    }
    let msg_type = r.read_u8().await?;
    if msg_type != MSG_HANDSHAKE {
        return Err(ProtoError::BadMessageType(msg_type));
    }
    let user_id = read_string(r).await?;
    let device_id = read_string(r).await?;
    let timestamp = r.read_i64().await?;
    let nonce_len = r.read_u8().await? as usize;
    if nonce_len != NONCE_LEN {
        return Err(ProtoError::Malformed("bad nonce length"));
    }
    let mut nonce = [0u8; NONCE_LEN];
    r.read_exact(&mut nonce).await?;
    let mut signature = [0u8; 64];
    r.read_exact(&mut signature).await?;
    Ok(Handshake { user_id, device_id, timestamp, nonce, signature })
}

/// Write a connect frame.
pub async fn write_connect<W: AsyncWrite + Unpin>(w: &mut W, c: &Connect) -> Result<(), ProtoError> {
    w.write_u8(PROTOCOL_VERSION).await?;
    w.write_u8(MSG_CONNECT).await?;
    write_string(w, &c.host).await?;
    w.write_u16(c.port).await?;
    w.flush().await?;
    Ok(())
}

/// Read a connect frame.
pub async fn read_connect<R: AsyncRead + Unpin>(r: &mut R) -> Result<Connect, ProtoError> {
    let version = r.read_u8().await?;
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::BadVersion(version));
    }
    let msg_type = r.read_u8().await?;
    if msg_type != MSG_CONNECT {
        return Err(ProtoError::BadMessageType(msg_type));
    }
    let host = read_string(r).await?;
    let port = r.read_u16().await?;
    Ok(Connect { host, port })
}

/// Write the terminal response frame.
pub async fn write_response<W: AsyncWrite + Unpin>(
    w: &mut W,
    result: Result<(), RejectReason>,
) -> Result<(), ProtoError> {
    w.write_u8(PROTOCOL_VERSION).await?;
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
    if version != PROTOCOL_VERSION {
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
