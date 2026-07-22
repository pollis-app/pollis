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
//! │ u8   version            (== PROTOCOL_VERSION)              │
//! │ u8   msg_type           (== MSG_HANDSHAKE)                 │
//! │ u16  user_id_len | user_id bytes            (UTF-8)        │
//! │ u16  device_id_len | device_id bytes        (UTF-8)       │
//! │ [32] device_signing_pub (Ed25519, PRESENTED not resolved) │
//! │ [32] account_id_pub     (Ed25519 account identity key)    │
//! │ u32  identity_version   (big-endian)                      │
//! │ u64  issued_at          (big-endian, unix seconds)        │
//! │ [64] device_cert        (account key's sig over the chain)│
//! │ i64  timestamp          (unix seconds, big-endian)        │
//! │ u8   nonce_len (== 32) | nonce bytes                      │
//! │ [64] signature          (device key, over the canonical)  │
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
//! # Handshake auth — OFFLINE device-certificate chain (design §9.4, §11.1)
//!
//! Unlike Slice 1 (which resolved the device key from an in-memory table), the
//! client now **presents its full identity chain** and the relay verifies it with
//! **zero I/O** — no Turso query, no network call per connection. That is the
//! mechanism that keeps the relay tier out of the metadata plane (§11.1). Two
//! independent checks, both must pass:
//!
//! 1. **Possession.** The handshake `signature` verifies, under the presented
//!    `device_signing_pub`, over the canonical message (skew-bounded, nonce'd):
//!    ```text
//!    pollis-relay-v2\n{user_id}\n{device_id}\n{timestamp}\n{sha256_hex(nonce)}
//!    ```
//!    ⇒ the connecting party holds that device signing private key.
//! 2. **Membership.** [`pollis_device_cert::verify_device_cert`] confirms
//!    `device_cert` is the account key `account_id_pub`'s signature binding
//!    `device_signing_pub` to this `device_id` at `identity_version` / `issued_at`
//!    ⇒ that device key was certified by the account.
//!
//! Together: "a cryptographically self-consistent Pollis device." v0 does NOT
//! anchor `account_id_pub` to the account-key transparency log (that needs a
//! fetch, which would re-introduce network coupling) — see `docs/relay-operations.md`.
//! Because destinations are allowlisted to first-party hosts only (§1.2), "a
//! well-formed device, rate-limited" is sufficient anti-abuse for v0.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wire protocol version. Bumped from 1 → 2 for the cert-chain handshake (Slice
/// 2a): the frame now carries the presented device key + the account cert chain.
pub const PROTOCOL_VERSION: u8 = 2;

/// ALPN for the outer relay-hop QUIC transport. Bumped alongside
/// [`PROTOCOL_VERSION`] so a v1 client cannot silently half-speak to a v2 relay.
pub const ALPN: &[u8] = b"pollis-relay/2";

/// Domain-separation prefix for the handshake canonical message.
pub const HANDSHAKE_DOMAIN: &str = "pollis-relay-v2";

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
    /// The user's Ed25519 account identity public key (32 bytes).
    pub account_id_pub: [u8; 32],
    /// The 64-byte cert: the account key's signature binding this device's
    /// signing key to `account_id_pub`.
    pub device_cert: [u8; 64],
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
        account: &SigningKey,
        device_id: &str,
        device_signing_pub: &[u8; 32],
        identity_version: u32,
        issued_at: u64,
    ) -> DeviceCertMaterial {
        let payload = pollis_device_cert::device_cert_signed_payload(
            device_id,
            device_signing_pub,
            identity_version,
            issued_at,
        )
        .expect("device_id / signing pub within cert length bounds");
        let sig: Signature = account.sign(&payload);
        DeviceCertMaterial {
            account_id_pub: account.verifying_key().to_bytes(),
            device_cert: sig.to_bytes(),
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
    /// The device's Ed25519 signing public key, PRESENTED in-band. The handshake
    /// `signature` is verified against it; the `device_cert` chains it to the
    /// account. (In Pollis this is `user_device.mls_signature_pub`.)
    pub device_signing_pub: [u8; 32],
    pub cert: DeviceCertMaterial,
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

/// What a verified handshake yields the relay: the authenticated `user_id` plus
/// the `account_id_pub` the rate limiter keys per-account limits on.
#[derive(Debug, Clone)]
pub struct VerifiedClient {
    pub user_id: String,
    pub account_id_pub: [u8; 32],
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
/// fresh `nonce`, signed by the device's Ed25519 private key, carrying the
/// device's account cert chain.
pub fn sign_handshake(
    signing_key: &SigningKey,
    user_id: &str,
    device_id: &str,
    cert: DeviceCertMaterial,
    timestamp: i64,
    nonce: [u8; NONCE_LEN],
) -> Handshake {
    let msg = handshake_canonical_bytes(user_id, device_id, timestamp, &nonce);
    let signature: Signature = signing_key.sign(&msg);
    Handshake {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        device_signing_pub: signing_key.verifying_key().to_bytes(),
        cert,
        timestamp,
        nonce,
        signature: signature.to_bytes(),
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
    let device_key =
        VerifyingKey::from_bytes(&hs.device_signing_pub).map_err(|_| RejectReason::Unauthorized)?;
    let msg = handshake_canonical_bytes(&hs.user_id, &hs.device_id, hs.timestamp, &hs.nonce);
    let signature = Signature::from_bytes(&hs.signature);
    device_key
        .verify(&msg, &signature)
        .map_err(|_| RejectReason::Unauthorized)?;

    // (b) Membership: the account key certified this device key (offline chain).
    pollis_device_cert::verify_device_cert(
        &hs.cert.account_id_pub,
        &hs.device_id,
        &hs.device_signing_pub,
        hs.cert.identity_version,
        hs.cert.issued_at,
        &hs.cert.device_cert,
    )
    .map_err(|_| RejectReason::Unauthorized)?;

    Ok(VerifiedClient {
        user_id: hs.user_id.clone(),
        account_id_pub: hs.cert.account_id_pub,
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

/// Write a handshake frame.
pub async fn write_handshake<W: AsyncWrite + Unpin>(w: &mut W, hs: &Handshake) -> Result<(), ProtoError> {
    w.write_u8(PROTOCOL_VERSION).await?;
    w.write_u8(MSG_HANDSHAKE).await?;
    write_string(w, &hs.user_id).await?;
    write_string(w, &hs.device_id).await?;
    w.write_all(&hs.device_signing_pub).await?;
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
    let mut device_signing_pub = [0u8; 32];
    r.read_exact(&mut device_signing_pub).await?;
    let mut account_id_pub = [0u8; 32];
    r.read_exact(&mut account_id_pub).await?;
    let identity_version = r.read_u32().await?;
    let issued_at = r.read_u64().await?;
    let mut device_cert = [0u8; 64];
    r.read_exact(&mut device_cert).await?;
    let timestamp = r.read_i64().await?;
    let nonce_len = r.read_u8().await? as usize;
    if nonce_len != NONCE_LEN {
        return Err(ProtoError::Malformed("bad nonce length"));
    }
    let mut nonce = [0u8; NONCE_LEN];
    r.read_exact(&mut nonce).await?;
    let mut signature = [0u8; 64];
    r.read_exact(&mut signature).await?;
    Ok(Handshake {
        user_id,
        device_id,
        device_signing_pub,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_chain_accepts() {
        let account = SigningKey::from_bytes(&[5u8; 32]);
        let device = SigningKey::from_bytes(&[6u8; 32]);
        let cert = DeviceCertMaterial::mint(&account, "d1", &device.verifying_key().to_bytes(), 1, 1_700_000_000);
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", cert, now, [1u8; 32]);
        let v = verify_handshake(&hs, now).expect("valid chain");
        assert_eq!(v.user_id, "u1");
        assert_eq!(v.account_id_pub, account.verifying_key().to_bytes());
    }

    #[test]
    fn forged_cert_rejected() {
        // Device key NOT certified by the presented account: the attacker signs a
        // cert with the WRONG (their own) account key but claims the victim's
        // account_id_pub.
        let victim_account = SigningKey::from_bytes(&[5u8; 32]);
        let attacker_account = SigningKey::from_bytes(&[9u8; 32]);
        let device = SigningKey::from_bytes(&[6u8; 32]);
        let mut cert =
            DeviceCertMaterial::mint(&attacker_account, "d1", &device.verifying_key().to_bytes(), 1, 1);
        // Claim the victim's account id, but the signature is the attacker's.
        cert.account_id_pub = victim_account.verifying_key().to_bytes();
        let now = now_unix();
        let hs = sign_handshake(&device, "u1", "d1", cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn cert_for_other_device_key_rejected() {
        // A valid cert, but the handshake is signed by a DIFFERENT device key than
        // the cert binds — possession and membership refer to different keys.
        let account = SigningKey::from_bytes(&[5u8; 32]);
        let real_device = SigningKey::from_bytes(&[6u8; 32]);
        let other_device = SigningKey::from_bytes(&[7u8; 32]);
        let cert = DeviceCertMaterial::mint(&account, "d1", &real_device.verifying_key().to_bytes(), 1, 1);
        let now = now_unix();
        // Sign with other_device; its pub is presented, but the cert binds real_device.
        let hs = sign_handshake(&other_device, "u1", "d1", cert, now, [1u8; 32]);
        assert_eq!(verify_handshake(&hs, now).unwrap_err(), RejectReason::Unauthorized);
    }

    #[test]
    fn expired_and_forged_signature_rejected() {
        let account = SigningKey::from_bytes(&[5u8; 32]);
        let device = SigningKey::from_bytes(&[6u8; 32]);
        let cert = DeviceCertMaterial::mint(&account, "d1", &device.verifying_key().to_bytes(), 1, 1);
        let now = now_unix();

        // Expired.
        let expired = sign_handshake(&device, "u1", "d1", cert.clone(), now - 10_000, [1u8; 32]);
        assert_eq!(verify_handshake(&expired, now).unwrap_err(), RejectReason::Unauthorized);

        // Forged signature byte.
        let mut forged = sign_handshake(&device, "u1", "d1", cert, now, [1u8; 32]);
        forged.signature[0] ^= 0xFF;
        assert_eq!(verify_handshake(&forged, now).unwrap_err(), RejectReason::Unauthorized);
    }
}
