//! The client's consumer of the signed relay directory (#616 §3, the frozen
//! contract; `infra/relay-hydra/lib/directory-verify.mjs` is the byte-for-byte
//! reference). The signature is verified over the exact bytes decoded from
//! `payload_b64`, then those bytes are parsed: no canonicalization.
//!
//! Every [`DirectoryError`] is fail-closed (no usable relays). The fetch is
//! direct, never through the overlay: the directory is what builds the pool.

use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// Timeout for one directory fetch; the pool keeps its membership meanwhile.
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

/// The artifact name a directory payload identifies itself by (cf.
/// `pollis_relay::REVOCATION_TYPE`).
///
/// The same pinned key signs the directory and `revocations.json`, so a valid
/// signature does not prove which artifact it is; a present-and-wrong `type`
/// is rejected. An absent one is accepted, because the reconciler publishes
/// none and requiring it would make client and reconciler deploys race. Every
/// other artifact this key signs carries a `type`, so absence is unambiguous.
pub const DIRECTORY_TYPE: &str = "pollis-relay-directory";

/// The signed envelope: base64 of the payload bytes plus base64 of the
/// Ed25519 signature over those exact bytes.
#[derive(Deserialize)]
struct Envelope {
    payload_b64: String,
    signature_b64: String,
}

/// The Directory object (the exact bytes that get base64'd into `payload_b64`).
#[derive(Debug, Clone, Deserialize)]
pub struct Directory {
    pub version: u32,
    /// The artifact self-identifier (JSON `type`); `None` before
    /// [`DIRECTORY_TYPE`] existed.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    pub issued_at: i64,
    pub expires_at: i64,
    pub relays: Vec<DirectoryRelay>,
    /// #813 revocation anchor; `None` when the pool has not enabled it.
    /// Additive and optional: `version` stays `1`, since a bump would make every
    /// shipped client reject the directory (see [`crate::net::revocation`]).
    #[serde(default)]
    pub revocation: Option<RevocationAnchor>,
    /// #813 peer-hosted relays, additive on the same terms. A `Vec`, because
    /// absent means "no peers", never "unknown".
    #[serde(default)]
    pub peers: Vec<DirectoryPeer>,
}

/// The directory's binding to the short-lived `revocations.json`: carrying the
/// sequence inside the signed directory detects a fresh directory served
/// beside a stale-but-unexpired list.
#[derive(Debug, Clone, Deserialize)]
pub struct RevocationAnchor {
    /// Monotonic publisher sequence; a list below it is a replay.
    pub seq: u64,
    /// Where the list lives, relative to the directory URL unless absolute.
    #[serde(default)]
    pub path: Option<String>,
    /// How many relays the anchored list revokes. Only `Some(0)` tolerates an
    /// unreachable list.
    #[serde(default)]
    pub count: Option<u32>,
}

/// The default artifact name, matching the reconciler's `REVOCATIONS_KEY`.
const DEFAULT_REVOCATION_PATH: &str = "revocations.json";

impl RevocationAnchor {
    /// The path to fetch, defaulted; an empty string counts as absent (as in
    /// `directory-verify.mjs`).
    pub fn resolved_path(&self) -> &str {
        match self.path.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => p,
            _ => DEFAULT_REVOCATION_PATH,
        }
    }
}

/// One relay advertised by the directory. `cert_b64` is base64 of the DER of
/// its pinned QUIC leaf, which is the relay's identity.
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryRelay {
    /// Directly-dialable `public-ip-or-host:udp-port`.
    pub addr: String,
    /// Informational; defaulted so a directory omitting it still parses.
    #[serde(default)]
    pub region: String,
    pub cert_b64: String,
}

/// One peer-hosted relay (#813). No `addr`: a peer is reached only through a
/// first-party relay it parked an outbound link with. No id or region: a relay
/// identity must not be linkable to an account; the cert is the identity.
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryPeer {
    /// Base64 DER of the peer's relay leaf, so the relay carrying the parked
    /// link cannot substitute itself.
    pub cert_b64: String,
    /// The first-party relays this peer is parked at; an entry with none is
    /// unreachable and dropped. Informational in v1 (peers park everywhere),
    /// but [`crate::net::overlay`] already honours it.
    #[serde(default)]
    pub parked_at: Vec<String>,
}

/// Fetch the raw envelope bytes from `url` directly (not through the overlay).
pub async fn fetch_directory(url: &str) -> Result<Vec<u8>, DirectoryError> {
    fetch_signed(url).await
}

/// Fetch any of the pool's signed artifacts over the shared direct client.
pub async fn fetch_signed(url: &str) -> Result<Vec<u8>, DirectoryError> {
    // The shared client reuses connections across the back-to-back directory
    // and revocation fetches. Per-request timeout, so it doesn't leak onto
    // other direct callers.
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

/// Verify a directory envelope against the pinned public key (base64 of the raw
/// 32 bytes) and validity window. `now_secs` is injected for tests.
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
    // Before the range checks, so a foreign artifact is rejected as such.
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

    /// Build a signed envelope from exact payload bytes, valid or not.
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

    /// A directory naming its own type is accepted; the name is what the
    /// reconciler publishes.
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

    /// A directory predating the field still verifies.
    #[test]
    fn accepts_a_directory_that_predates_the_artifact_type() {
        let sk = signing_key();
        let dir = verify_directory(&envelope_for(&sk, &valid_payload(2000)), &pinned_b64(&sk), 1500)
            .expect("valid");
        assert!(dir.kind.is_none());
    }

    /// A genuine revocation list served as the directory verifies but must be
    /// rejected as the wrong artifact. The payload carries every field
    /// `Directory` needs, so only the `type` check can reject it.
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

    /// Any other artifact name is rejected too: an allowlist of one.
    #[test]
    fn rejects_an_unknown_artifact_type() {
        let sk = signing_key();
        let payload = r#"{"version":1,"type":"pollis-something-else","issued_at":1000,"expires_at":2000,"relays":[{"addr":"x:1","cert_b64":"QUJD"}]}"#;
        assert!(matches!(
            verify_directory(&envelope_for(&sk, payload), &pinned_b64(&sk), 1500),
            Err(DirectoryError::WrongType(_))
        ));
    }

    /// The mirror case: the revocation verifier rejects a directory. Both halves
    /// live here so the pair cannot drift.
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

    /// A directory without the #813 anchor still verifies; `version` stays 1.
    #[test]
    fn a_directory_without_a_revocation_anchor_still_verifies() {
        let sk = signing_key();
        let dir = verify_directory(&envelope_for(&sk, &valid_payload(2000)), &pinned_b64(&sk), 1500)
            .expect("valid");
        assert_eq!(dir.version, 1);
        assert!(dir.revocation.is_none());
    }

    /// A directory with the anchor parses it, defaulting `path`.
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

    /// A directory without `peers` still verifies and means "no peers".
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

    /// A directory with `peers` parses them, defaulting `parked_at`.
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
        // A peer with no parking is parsed; the client drops it later.
        assert!(dir.peers[1].parked_at.is_empty());
    }

    /// A directory carrying both additive fields still satisfies a shipped
    /// client (JS half: `infra/relay-hydra/test/directory-contract.test.mjs`).
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

    /// Cross-language interop: verify a real reconciler-signed directory (Node
    /// `crypto.sign`). Ignored (needs the live pool); run with:
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
        crate::diag!(
            "live directory OK: version={} relays={} expires_at={}",
            dir.version,
            dir.relays.len(),
            dir.expires_at
        );
        assert!(!dir.relays.is_empty());
    }
}
