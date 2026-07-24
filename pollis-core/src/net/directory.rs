//! The client's consumer of the signed relay **directory** (issue #616 §3, the
//! frozen contract; the hydra's `infra/relay-hydra/lib/directory-verify.mjs` is
//! the byte-for-byte reference this mirrors). The reconciler Lambda signs a
//! directory of live relay endpoints and publishes it at a stable HTTPS URL; this
//! module fetches it, verifies the Ed25519 signature against the client-pinned
//! public key, and hands back the typed [`Directory`]. The overlay engine
//! ([`crate::net::overlay`]) turns that into the live relay pool.
//!
//! **Byte-for-byte discipline (§3).** The envelope is
//! `{ "payload_b64", "signature_b64" }`. We verify the signature over the EXACT
//! bytes we base64-decode from `payload_b64`, THEN parse those bytes as the
//! Directory. No JSON canonicalization — both sides sign/verify identical bytes.
//!
//! **Fail closed.** Every rejection ([`DirectoryError`]) leaves the caller with no
//! usable relays, which in `Prefer` means direct fallback and in `Strict` a
//! surfaced degrade — never a silent send over an unverified path. The reject set
//! matches the reference exactly: bad signature, `version != 1`, `now >=
//! expires_at`, malformed JSON (envelope or payload), or empty `relays`.
//!
//! **Bootstrap is direct, by necessity.** The fetch uses a plain (non-overlay)
//! HTTP client: the directory is what BUILDS the relay pool, so it cannot itself
//! route through a pool that does not exist yet. The artifact is Ed25519-signed,
//! so its integrity does not depend on the transport — only the client's IP is
//! exposed to the directory host during the fetch, the same unavoidable bootstrap
//! every bridge-list design has.

use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// How long a single directory fetch may take before it is treated as a failure
/// (the refresh loop retries; the pool keeps its previous membership meanwhile).
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a directory was rejected. Every variant is fail-closed at the call site.
#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("directory fetch failed: {0}")]
    Fetch(String),
    #[error("malformed envelope JSON")]
    MalformedEnvelope,
    #[error("malformed payload JSON")]
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
    #[error("directory expired (now {now} >= expires_at {expires_at})")]
    Expired { now: i64, expires_at: i64 },
    #[error("directory lists no relays")]
    EmptyRelays,
}

/// The signed envelope as published: base64 (STANDARD) of the payload bytes plus
/// base64 of the 64-byte Ed25519 signature over those exact bytes.
#[derive(Deserialize)]
struct Envelope {
    payload_b64: String,
    signature_b64: String,
}

/// The Directory object (the exact bytes that get base64'd into `payload_b64`).
#[derive(Debug, Clone, Deserialize)]
pub struct Directory {
    pub version: u32,
    pub issued_at: i64,
    pub expires_at: i64,
    pub relays: Vec<DirectoryRelay>,
}

/// One relay advertised by the directory. `cert_b64` is base64 (STANDARD) of the
/// DER bytes of that node's pinned QUIC leaf — the client verifies the relay it
/// dials against exactly this cert (the relay's identity *is* its cert, §7).
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryRelay {
    /// Directly-dialable `public-ip-or-host:udp-port`.
    pub addr: String,
    /// Informational (the client may later prefer nearer regions). Defaulted so a
    /// future directory that omits it still parses.
    #[serde(default)]
    pub region: String,
    pub cert_b64: String,
}

/// Fetch the raw envelope bytes from `url` over a DIRECT (non-overlay) client.
/// See the module docs for why the fetch cannot route through the overlay.
pub async fn fetch_directory(url: &str) -> Result<Vec<u8>, DirectoryError> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| DirectoryError::Fetch(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| DirectoryError::Fetch(e.to_string()))?
        .error_for_status()
        .map_err(|e| DirectoryError::Fetch(e.to_string()))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| DirectoryError::Fetch(e.to_string()))?;
    Ok(bytes.to_vec())
}

/// Verify a directory envelope against the pinned public key and validity window,
/// returning the parsed [`Directory`] on success. `pinned_pubkey_b64` is base64
/// (STANDARD) of the raw 32-byte Ed25519 public key (as
/// `POLLIS_OVERLAY_DIRECTORY_KEY` is minted). `now_secs` is injected so tests can
/// exercise expiry deterministically.
///
/// Mirrors `directory-verify.mjs` exactly: verify the signature over the decoded
/// payload bytes FIRST, then parse and range-check.
pub fn verify_directory(
    envelope_bytes: &[u8],
    pinned_pubkey_b64: &str,
    now_secs: i64,
) -> Result<Directory, DirectoryError> {
    let envelope: Envelope =
        serde_json::from_slice(envelope_bytes).map_err(|_| DirectoryError::MalformedEnvelope)?;

    let payload = base64::engine::general_purpose::STANDARD
        .decode(envelope.payload_b64.as_bytes())
        .map_err(|_| DirectoryError::BadBase64("payload_b64"))?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(envelope.signature_b64.as_bytes())
        .map_err(|_| DirectoryError::BadBase64("signature_b64"))?;

    let verifying_key = verifying_key_from_b64(pinned_pubkey_b64)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| DirectoryError::BadSignatureLen)?;
    verifying_key
        .verify_strict(&payload, &signature)
        .map_err(|_| DirectoryError::BadSignature)?;

    let directory: Directory =
        serde_json::from_slice(&payload).map_err(|_| DirectoryError::MalformedPayload)?;

    if directory.version != 1 {
        return Err(DirectoryError::UnsupportedVersion(directory.version));
    }
    if now_secs >= directory.expires_at {
        return Err(DirectoryError::Expired {
            now: now_secs,
            expires_at: directory.expires_at,
        });
    }
    if directory.relays.is_empty() {
        return Err(DirectoryError::EmptyRelays);
    }
    Ok(directory)
}

/// Rebuild an Ed25519 [`VerifyingKey`] from the base64 of its raw 32 bytes.
fn verifying_key_from_b64(pinned_pubkey_b64: &str) -> Result<VerifyingKey, DirectoryError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(pinned_pubkey_b64.trim().as_bytes())
        .map_err(|_| DirectoryError::BadBase64("pinned key"))?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| DirectoryError::BadPinnedKey)?;
    VerifyingKey::from_bytes(&raw).map_err(|_| DirectoryError::BadPinnedKey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // Deterministic key so the test never touches the RNG.
    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn pinned_b64(sk: &SigningKey) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
    }

    /// Build a signed envelope from a Directory JSON string (so tests can craft
    /// exact payload bytes, including invalid ones, and sign them faithfully).
    fn envelope_for(sk: &SigningKey, payload_json: &str) -> Vec<u8> {
        let payload = payload_json.as_bytes();
        let sig = sk.sign(payload);
        let env = serde_json::json!({
            "payload_b64": base64::engine::general_purpose::STANDARD.encode(payload),
            "signature_b64": base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        });
        serde_json::to_vec(&env).unwrap()
    }

    fn valid_payload(expires_at: i64) -> String {
        format!(
            r#"{{"version":1,"issued_at":1000,"expires_at":{expires_at},"relays":[{{"addr":"203.0.113.7:9444","region":"us-west-2","cert_b64":"QUJD"}},{{"addr":"203.0.113.8:9444","region":"us-west-2","cert_b64":"REVG"}}]}}"#
        )
    }

    #[test]
    fn accepts_a_valid_directory_with_per_node_certs() {
        let sk = signing_key();
        let env = envelope_for(&sk, &valid_payload(2000));
        let dir = verify_directory(&env, &pinned_b64(&sk), 1500).expect("valid");
        assert_eq!(dir.version, 1);
        assert_eq!(dir.relays.len(), 2);
        assert_eq!(dir.relays[0].addr, "203.0.113.7:9444");
        // Per-node certs are distinct — the format carries one per entry.
        assert_ne!(dir.relays[0].cert_b64, dir.relays[1].cert_b64);
    }

    #[test]
    fn rejects_a_tampered_payload() {
        let sk = signing_key();
        let mut env = envelope_for(&sk, &valid_payload(2000));
        // Flip a byte in the base64 payload so the signature no longer matches.
        let text = String::from_utf8(env).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let p = v["payload_b64"].as_str().unwrap().to_string();
        let mut chars: Vec<char> = p.chars().collect();
        chars[10] = if chars[10] == 'A' { 'B' } else { 'A' };
        v["payload_b64"] = serde_json::Value::String(chars.into_iter().collect());
        env = serde_json::to_vec(&v).unwrap();
        assert!(matches!(
            verify_directory(&env, &pinned_b64(&sk), 1500),
            Err(DirectoryError::BadSignature
                | DirectoryError::MalformedPayload
                | DirectoryError::BadBase64(_))
        ));
    }

    #[test]
    fn rejects_signature_from_the_wrong_key() {
        let sk = signing_key();
        let env = envelope_for(&sk, &valid_payload(2000));
        let attacker = SigningKey::from_bytes(&[9u8; 32]);
        assert!(matches!(
            verify_directory(&env, &pinned_b64(&attacker), 1500),
            Err(DirectoryError::BadSignature)
        ));
    }

    #[test]
    fn rejects_unsupported_version() {
        let sk = signing_key();
        let payload = r#"{"version":2,"issued_at":1000,"expires_at":2000,"relays":[{"addr":"x:1","cert_b64":"QUJD"}]}"#;
        let env = envelope_for(&sk, payload);
        assert!(matches!(
            verify_directory(&env, &pinned_b64(&sk), 1500),
            Err(DirectoryError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn rejects_an_expired_directory() {
        let sk = signing_key();
        let env = envelope_for(&sk, &valid_payload(2000));
        // now == expires_at is already expired (>=, matching the reference).
        assert!(matches!(
            verify_directory(&env, &pinned_b64(&sk), 2000),
            Err(DirectoryError::Expired { .. })
        ));
    }

    #[test]
    fn rejects_empty_relays() {
        let sk = signing_key();
        let payload = r#"{"version":1,"issued_at":1000,"expires_at":2000,"relays":[]}"#;
        let env = envelope_for(&sk, payload);
        assert!(matches!(
            verify_directory(&env, &pinned_b64(&sk), 1500),
            Err(DirectoryError::EmptyRelays)
        ));
    }

    #[test]
    fn rejects_malformed_envelope() {
        let sk = signing_key();
        assert!(matches!(
            verify_directory(b"not json", &pinned_b64(&sk), 1500),
            Err(DirectoryError::MalformedEnvelope)
        ));
    }

    /// Cross-language interop proof: verify a REAL reconciler-signed directory
    /// (Node `crypto.sign`) with this Rust verifier — the unit tests above only
    /// prove dalek-signs / dalek-verifies. Ignored (needs the live pool + creds);
    /// run locally with:
    ///   POLLIS_OVERLAY_DIRECTORY_URL=... POLLIS_OVERLAY_DIRECTORY_KEY=... \
    ///     cargo test -p pollis-core --no-default-features --features test-harness \
    ///     net::directory::tests::verifies_the_live_directory -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn verifies_the_live_directory() {
        let url = std::env::var("POLLIS_OVERLAY_DIRECTORY_URL").expect("set URL");
        let key = std::env::var("POLLIS_OVERLAY_DIRECTORY_KEY").expect("set KEY");
        let bytes = super::fetch_directory(&url).await.expect("fetch");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let dir = verify_directory(&bytes, &key, now).expect("live directory must verify");
        eprintln!(
            "live directory OK: version={} relays={} expires_at={}",
            dir.version,
            dir.relays.len(),
            dir.expires_at
        );
        assert!(!dir.relays.is_empty());
    }
}
