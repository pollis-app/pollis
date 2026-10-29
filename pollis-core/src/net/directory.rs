//! Fetches and verifies the signed relay **directory** (#616 §3; reference: `directory-verify.mjs`).
//!
//! **Byte-for-byte (§3).** Verify the signature over the decoded `payload_b64` bytes, THEN parse them.
//!
//! **Fail closed.** Every [`DirectoryError`] leaves no usable relays; the reject set matches the reference.
//!
//! **Bootstrap is direct**: the directory builds the pool, so it cannot route through it.

use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// Per-fetch timeout; the pool keeps its membership meanwhile.
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
    #[error("wrong artifact type {0:?} (expected {DIRECTORY_TYPE:?} or none)")]
    WrongType(String),
}

/// The artifact name a directory payload identifies itself by, mirroring
/// `pollis_relay::REVOCATION_TYPE` for the revocation list.
///
/// **Cross-artifact guard.** The same key signs the directory and `revocations.json`, so a signature alone can't tell them apart.
///
/// **One-sided on purpose**: a wrong `type` is rejected, an absent one accepted, so no reconciler deploy-order race.
///
/// Safe because every other artifact this key signs carries a `type`.
pub const DIRECTORY_TYPE: &str = "pollis-relay-directory";

/// The signed envelope: base64 payload bytes plus a base64 Ed25519 signature over them.
#[derive(Deserialize)]
struct Envelope {
    payload_b64: String,
    signature_b64: String,
}

/// The Directory object (the exact bytes that get base64'd into `payload_b64`).
#[derive(Debug, Clone, Deserialize)]
pub struct Directory {
    pub version: u32,
    /// JSON `type`; `None` before [`DIRECTORY_TYPE`] existed (see there).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    pub issued_at: i64,
    pub expires_at: i64,
    pub relays: Vec<DirectoryRelay>,
    /// #813 revocation anchor; `None` when not enabled.
    ///
    /// Additive and optional; `version` stays `1` so shipped clients keep accepting the directory.
    #[serde(default)]
    pub revocation: Option<RevocationAnchor>,
    /// #813 wave-3 peer-hosted relays.
    ///
    /// Additive, `version` stays `1`. **Absent means "no peers"**, hence a defaulted `Vec`.
    #[serde(default)]
    pub peers: Vec<DirectoryPeer>,
}

/// The directory's binding to the separately-published, short-lived
/// `revocations.json`.
///
/// Carrying the sequence inside the signed directory detects a fresh directory paired with a stale list.
#[derive(Debug, Clone, Deserialize)]
pub struct RevocationAnchor {
    /// Monotonic publisher sequence; a list below this is a replay.
    pub seq: u64,
    /// Where the list lives, relative to the directory URL unless absolute.
    #[serde(default)]
    pub path: Option<String>,
    /// How many relays the list revokes. Only `Some(0)` tolerates an unreachable list.
    #[serde(default)]
    pub count: Option<u32>,
}

/// The default artifact name, matching the reconciler's `REVOCATIONS_KEY`.
const DEFAULT_REVOCATION_PATH: &str = "revocations.json";

impl RevocationAnchor {
    /// The path to fetch; an empty string counts as absent.
    pub fn resolved_path(&self) -> &str {
        match self.path.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => p,
            _ => DEFAULT_REVOCATION_PATH,
        }
    }
}

/// One relay; `cert_b64` is its pinned QUIC leaf (the cert is the identity, §7).
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryRelay {
    /// Directly-dialable `public-ip-or-host:udp-port`.
    pub addr: String,
    /// Informational; defaulted.
    #[serde(default)]
    pub region: String,
    pub cert_b64: String,
}

/// One **peer-hosted** relay (#813 wave 3). Deliberately unlike a [`DirectoryRelay`]:
///
/// - **no `addr`**: a peer never listens; it is reached only through a relay it parked at;
/// - **nothing about the volunteer**: the cert IS the identity (§7).
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryPeer {
    /// Base64 DER of the peer's relay leaf; this hop's layer is pinned to it.
    pub cert_b64: String,
    /// First-party relays this peer is parked at; an entry with none is dropped.
    #[serde(default)]
    pub parked_at: Vec<String>,
}

/// Fetch the raw envelope bytes from `url` over a DIRECT (non-overlay) client.
pub async fn fetch_directory(url: &str) -> Result<Vec<u8>, DirectoryError> {
    fetch_signed(url).await
}

/// Fetch any signed pool artifact over the same DIRECT client.
pub async fn fetch_signed(url: &str) -> Result<Vec<u8>, DirectoryError> {
    // The shared direct client keeps its connection pool across back-to-back fetches; timeout is per-request.
    let resp = pollis_relay::http::http_client(None)
        .get(url)
        .timeout(FETCH_TIMEOUT)
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
/// `pinned_pubkey_b64` is the raw 32-byte key in base64; `now_secs` is injected for tests.
///
/// Mirrors `directory-verify.mjs`: verify signature FIRST, then parse and range-check.
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
    // Checked before range checks so a foreign artifact is rejected as such.
    if let Some(kind) = directory.kind.as_deref() {
        if kind != DIRECTORY_TYPE {
            return Err(DirectoryError::WrongType(kind.to_string()));
        }
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

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn pinned_b64(sk: &SigningKey) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
    }

    /// Sign a Directory JSON string as-is, so tests control the exact bytes.
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
        assert_ne!(dir.relays[0].cert_b64, dir.relays[1].cert_b64);
    }

    #[test]
    fn rejects_a_tampered_payload() {
        let sk = signing_key();
        let mut env = envelope_for(&sk, &valid_payload(2000));
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
        // now == expires_at is already expired.
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

    /// Cross-artifact, positive case: the published name verifies.
    #[test]
    fn accepts_a_directory_that_names_its_own_artifact_type() {
        let sk = signing_key();
        let payload = format!(
            r#"{{"version":1,"type":"{DIRECTORY_TYPE}","issued_at":1000,"expires_at":2000,"relays":[{{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}}]}}"#
        );
        let dir =
            verify_directory(&envelope_for(&sk, &payload), &pinned_b64(&sk), 1500).expect("valid");
        assert_eq!(dir.kind.as_deref(), Some(DIRECTORY_TYPE));
        assert_eq!(dir.relays.len(), 1);
    }

    /// A directory without the field still verifies.
    #[test]
    fn accepts_a_directory_that_predates_the_artifact_type() {
        let sk = signing_key();
        let dir = verify_directory(&envelope_for(&sk, &valid_payload(2000)), &pinned_b64(&sk), 1500)
            .expect("valid");
        assert!(dir.kind.is_none());
    }

    /// Cross-artifact attack: a same-key revocation list with every Directory field is rejected by `type` alone.
    #[test]
    fn rejects_a_revocation_list_signed_by_the_same_key() {
        let sk = signing_key();
        let payload = format!(
            r#"{{"version":1,"type":"{}","seq":4,"issued_at":1000,"expires_at":2000,"revoked":[{{"addr":"203.0.113.7:9444"}}],"relays":[{{"addr":"203.0.113.7:9444","cert_b64":"QUJD"}}]}}"#,
            pollis_relay::REVOCATION_TYPE
        );
        match verify_directory(&envelope_for(&sk, &payload), &pinned_b64(&sk), 1500) {
            Err(DirectoryError::WrongType(k)) => {
                assert_eq!(k, pollis_relay::REVOCATION_TYPE);
            }
            other => panic!("a same-key revocation list must be rejected as the wrong artifact, got {other:?}"),
        }
    }

    /// The guard is an allowlist of one.
    #[test]
    fn rejects_an_unknown_artifact_type() {
        let sk = signing_key();
        let payload = r#"{"version":1,"type":"pollis-something-else","issued_at":1000,"expires_at":2000,"relays":[{"addr":"x:1","cert_b64":"QUJD"}]}"#;
        assert!(matches!(
            verify_directory(&envelope_for(&sk, payload), &pinned_b64(&sk), 1500),
            Err(DirectoryError::WrongType(_))
        ));
    }

    /// Mirror case: the revocation verifier rejects a directory, kept here so the pair can't drift.
    #[test]
    fn the_revocation_verifier_rejects_a_directory_signed_by_the_same_key() {
        let sk = signing_key();
        let payload = format!(
            r#"{{"version":1,"type":"{DIRECTORY_TYPE}","seq":4,"issued_at":1000,"expires_at":2000,"revoked":[],"relays":[{{"addr":"x:1","cert_b64":"QUJD"}}]}}"#
        );
        let env = envelope_for(&sk, &payload);
        assert!(
            matches!(
                pollis_relay::verify_revocations(&env, &pinned_b64(&sk), 1500, 0),
                Err(pollis_relay::RevocationError::WrongType(_))
            ),
            "a same-key directory must be rejected by the revocation verifier"
        );

        let ok = format!(
            r#"{{"version":1,"type":"{}","seq":4,"issued_at":1000,"expires_at":2000,"revoked":[]}}"#,
            pollis_relay::REVOCATION_TYPE
        );
        pollis_relay::verify_revocations(&envelope_for(&sk, &ok), &pinned_b64(&sk), 1500, 0)
            .expect("a well-formed revocation list must still verify");
    }

    /// The #813 anchor is additive: a directory WITHOUT it still verifies.
    #[test]
    fn a_directory_without_a_revocation_anchor_still_verifies() {
        let sk = signing_key();
        let dir = verify_directory(&envelope_for(&sk, &valid_payload(2000)), &pinned_b64(&sk), 1500)
            .expect("valid");
        assert_eq!(dir.version, 1);
        assert!(dir.revocation.is_none());
    }

    /// ...and a directory WITH it parses the anchor, defaulting `path`.
    #[test]
    fn parses_the_revocation_anchor() {
        let sk = signing_key();
        let payload = r#"{"version":1,"issued_at":1000,"expires_at":2000,"relays":[{"addr":"x:1","cert_b64":"QUJD"}],"revocation":{"seq":12,"count":2}}"#;
        let dir =
            verify_directory(&envelope_for(&sk, payload), &pinned_b64(&sk), 1500).expect("valid");
        let anchor = dir.revocation.expect("anchor present");
        assert_eq!(anchor.seq, 12);
        assert_eq!(anchor.count, Some(2));
        assert_eq!(anchor.resolved_path(), "revocations.json");

        let payload = r#"{"version":1,"issued_at":1000,"expires_at":2000,"relays":[{"addr":"x:1","cert_b64":"QUJD"}],"revocation":{"seq":1,"path":"  ","count":0}}"#;
        let dir =
            verify_directory(&envelope_for(&sk, payload), &pinned_b64(&sk), 1500).expect("valid");
        assert_eq!(dir.revocation.unwrap().resolved_path(), "revocations.json");
    }

    /// `peers` is additive too: absent reads as "no peers".
    #[test]
    fn a_directory_without_peers_still_verifies_and_means_no_peers() {
        let sk = signing_key();
        let dir = verify_directory(&envelope_for(&sk, &valid_payload(2000)), &pinned_b64(&sk), 1500)
            .expect("valid");
        assert_eq!(dir.version, 1);
        assert!(
            dir.peers.is_empty(),
            "an absent `peers` must mean NO peers, never 'unknown'"
        );
    }

    /// ...and a directory WITH it parses peer entries, defaulting `parked_at`.
    #[test]
    fn parses_peer_entries() {
        let sk = signing_key();
        let payload = r#"{"version":1,"issued_at":1000,"expires_at":2000,"relays":[{"addr":"x:1","cert_b64":"QUJD"}],"peers":[{"cert_b64":"UEVFUg==","parked_at":["203.0.113.7:9444","203.0.113.8:9444"]},{"cert_b64":"UEVFUjI="}]}"#;
        let dir =
            verify_directory(&envelope_for(&sk, payload), &pinned_b64(&sk), 1500).expect("valid");
        assert_eq!(dir.version, 1);
        assert_eq!(dir.peers.len(), 2);
        assert_eq!(dir.peers[0].cert_b64, "UEVFUg==");
        assert_eq!(dir.peers[0].parked_at.len(), 2);
        // A peer with no parking is parsed and dropped by the client, not rejected.
        assert!(dir.peers[1].parked_at.is_empty());
    }

    /// Frozen §3 contract: both additive fields leave everything a shipped client checks intact.
    #[test]
    fn an_old_client_still_parses_a_peers_carrying_directory() {
        let sk = signing_key();
        let payload = r#"{"version":1,"issued_at":1000,"expires_at":2000,"relays":[{"addr":"203.0.113.7:9444","region":"us-west-2","cert_b64":"QUJD"}],"revocation":{"seq":9,"count":0},"peers":[{"cert_b64":"UEVFUg==","parked_at":["203.0.113.7:9444"]}],"some_field_from_2028":{"nested":true}}"#;
        let dir =
            verify_directory(&envelope_for(&sk, payload), &pinned_b64(&sk), 1500).expect("valid");
        assert_eq!(dir.version, 1, "version must stay 1 — see the module docs");
        assert_eq!(dir.relays.len(), 1);
        assert_eq!(dir.relays[0].addr, "203.0.113.7:9444");
        assert_eq!(dir.relays[0].region, "us-west-2");
        assert_eq!(dir.relays[0].cert_b64, "QUJD");
        assert_eq!(dir.revocation.expect("anchor").seq, 9);
        assert_eq!(dir.peers.len(), 1);
    }

    #[test]
    fn rejects_malformed_envelope() {
        let sk = signing_key();
        assert!(matches!(
            verify_directory(b"not json", &pinned_b64(&sk), 1500),
            Err(DirectoryError::MalformedEnvelope)
        ));
    }

    /// Cross-language proof against a real reconciler-signed directory. Ignored; run with:
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
        crate::diag!(
            "live directory OK: version={} relays={} expires_at={}",
            dir.version,
            dir.relays.len(),
            dir.expires_at
        );
        assert!(!dir.relays.is_empty());
    }
}
