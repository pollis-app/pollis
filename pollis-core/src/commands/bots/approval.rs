//! The host approval (#1182): what an admin signs so an existing host of a bot
//! will wrap the bot's account key to a new host.
//!
//! Canonical JSON (fields in sorted order, no whitespace, no unknown fields),
//! signed with the admin's device ML-DSA-44 key over
//! [`HOST_APPROVAL_DOMAIN`] `‖ 0x00 ‖ canonical`. On the wire (opaque to the DS)
//! it travels as [`SignedHostApproval`]: the canonical bytes and the signature,
//! both base64, so the verifier checks exactly the bytes that were signed and
//! then refuses any payload that is not canonical.

use base64::Engine as _;
use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa44, Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Domain separation for the approval signature.
pub const HOST_APPROVAL_DOMAIN: &[u8] = b"pollis-bot-host-approval-v1";

/// The approval format version.
pub const HOST_APPROVAL_VERSION: u32 = 1;

/// The longest an approval may live: ten minutes, the pairing request's TTL.
pub const HOST_APPROVAL_MAX_TTL_SECS: i64 = 10 * 60;

/// What the admin approves. Field order is the canonical (sorted) order; do not reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostApproval {
    pub approver_device_id: String,
    pub approver_user_id: String,
    pub bot_user_id: String,
    /// Unix seconds; at most [`HOST_APPROVAL_MAX_TTL_SECS`] after `issued_at`.
    pub expires_at: i64,
    pub group_id: String,
    /// Unix seconds.
    pub issued_at: i64,
    /// The new host's ephemeral X25519 key, base64 (STANDARD), as the admin's
    /// client read it when it checked the typed code.
    pub new_host_ephemeral_pub: String,
    pub request_id: String,
    pub v: u32,
}

/// The wire form, base64-encoded as a whole into `ApproveBotHostBody::host_approval`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedHostApproval {
    /// The canonical JSON of a [`HostApproval`], base64 (STANDARD).
    pub payload: String,
    /// ML-DSA-44 over `HOST_APPROVAL_DOMAIN ‖ 0x00 ‖ payload bytes`, base64 (STANDARD).
    pub signature: String,
}

/// Why an approval does not parse or verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecodeError {
    /// Not base64, not JSON, unknown fields, or a payload that is not canonical.
    Malformed(&'static str),
    /// The signature does not verify under the given key.
    BadSignature,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl HostApproval {
    /// The canonical bytes: compact JSON in field (sorted) order.
    pub fn canonical(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a HostApproval always serializes")
    }

    /// Sign with a device ML-DSA-44 key and encode for the DS.
    pub fn sign(&self, device_key: &SigningKey<MlDsa44>) -> String {
        sign_payload(HOST_APPROVAL_DOMAIN, &self.canonical(), device_key)
    }
}

/// `domain ‖ 0x00 ‖ payload`: what every admin-signed record (#1182 host
/// approvals, #2005 App approvals) is signed over.
pub(crate) fn signed_message_in(domain: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(domain.len() + 1 + payload.len());
    m.extend_from_slice(domain);
    m.push(0);
    m.extend_from_slice(payload);
    m
}

/// Sign `payload` under `domain` and encode it as a [`SignedHostApproval`]-shaped
/// wire record (`{payload, signature}`, base64 inside and out).
pub(crate) fn sign_payload(domain: &[u8], payload: &[u8], device_key: &SigningKey<MlDsa44>) -> String {
    let sig = device_key.sign(&signed_message_in(domain, payload)).encode().to_vec();
    let wire = SignedHostApproval {
        payload: b64().encode(payload),
        signature: b64().encode(sig),
    };
    b64().encode(serde_json::to_vec(&wire).expect("a SignedHostApproval always serializes"))
}

/// The `(payload, signature)` bytes of a wire record, not yet checked.
pub(crate) fn decode_wire(encoded: &str) -> Result<(Vec<u8>, Vec<u8>), ApprovalDecodeError> {
    let outer = b64().decode(encoded).map_err(|_| ApprovalDecodeError::Malformed("approval is not base64"))?;
    let wire: SignedHostApproval =
        serde_json::from_slice(&outer).map_err(|_| ApprovalDecodeError::Malformed("approval is not JSON"))?;
    let payload = b64()
        .decode(&wire.payload)
        .map_err(|_| ApprovalDecodeError::Malformed("payload is not base64"))?;
    let signature = b64()
        .decode(&wire.signature)
        .map_err(|_| ApprovalDecodeError::Malformed("signature is not base64"))?;
    Ok((payload, signature))
}

/// Parse an approval WITHOUT checking its signature, to learn who claims to have
/// signed it (so the verifier can look up that device's certified key).
pub fn decode_unverified(encoded: &str) -> Result<(HostApproval, Vec<u8>, Vec<u8>), ApprovalDecodeError> {
    let (payload, signature) = decode_wire(encoded)?;
    let approval: HostApproval =
        serde_json::from_slice(&payload).map_err(|_| ApprovalDecodeError::Malformed("payload is not an approval"))?;
    if approval.canonical() != payload {
        return Err(ApprovalDecodeError::Malformed("payload is not canonical"));
    }
    Ok((approval, payload, signature))
}

/// Verify a decoded approval's signature under the signer's ML-DSA-44 key.
pub fn verify_signature(pq_pub: &[u8], payload: &[u8], signature: &[u8]) -> Result<(), ApprovalDecodeError> {
    verify_signature_in(HOST_APPROVAL_DOMAIN, pq_pub, payload, signature)
}

/// Verify `signature` over `domain ‖ 0x00 ‖ payload` under an ML-DSA-44 key.
pub fn verify_signature_in(
    domain: &[u8],
    pq_pub: &[u8],
    payload: &[u8],
    signature: &[u8],
) -> Result<(), ApprovalDecodeError> {
    let encoded_key: &EncodedVerifyingKey<MlDsa44> =
        pq_pub.try_into().map_err(|_| ApprovalDecodeError::BadSignature)?;
    let encoded_sig: &EncodedSignature<MlDsa44> =
        signature.try_into().map_err(|_| ApprovalDecodeError::BadSignature)?;
    let vk = VerifyingKey::<MlDsa44>::decode(encoded_key);
    let sig = Signature::<MlDsa44>::decode(encoded_sig).ok_or(ApprovalDecodeError::BadSignature)?;
    vk.verify(&signed_message_in(domain, payload), &sig)
        .map_err(|_| ApprovalDecodeError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::Keypair as _;

    fn approval() -> HostApproval {
        HostApproval {
            approver_device_id: "DEV".into(),
            approver_user_id: "ADMIN".into(),
            bot_user_id: "BOT".into(),
            expires_at: 1_700_000_600,
            group_id: "GROUP".into(),
            issued_at: 1_700_000_000,
            new_host_ephemeral_pub: b64().encode([3u8; 32]),
            request_id: "REQ".into(),
            v: HOST_APPROVAL_VERSION,
        }
    }

    fn key(seed: u8) -> SigningKey<MlDsa44> {
        SigningKey::<MlDsa44>::from_seed(&[seed; 32].into())
    }

    #[test]
    fn the_canonical_form_is_sorted_compact_json() {
        let bytes = approval().canonical();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("{\"approver_device_id\":\"DEV\",\"approver_user_id\""));
        assert!(text.ends_with("\"request_id\":\"REQ\",\"v\":1}"));
        assert!(!text.contains(' '));
        // Sorted: re-sorting through a BTreeMap gives the same bytes.
        let v: std::collections::BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), text);
    }

    #[test]
    fn a_signed_approval_round_trips_under_the_signers_key_only() {
        let k = key(1);
        let encoded = approval().sign(&k);
        let (got, payload, sig) = decode_unverified(&encoded).unwrap();
        assert_eq!(got, approval());
        verify_signature(&k.verifying_key().encode(), &payload, &sig).unwrap();
        assert_eq!(
            verify_signature(&key(2).verifying_key().encode(), &payload, &sig),
            Err(ApprovalDecodeError::BadSignature)
        );
    }

    #[test]
    fn an_edited_payload_fails_the_signature() {
        let k = key(1);
        let encoded = approval().sign(&k);
        let (_, _, sig) = decode_unverified(&encoded).unwrap();
        let mut edited = approval();
        edited.expires_at += 3600;
        assert_eq!(
            verify_signature(&k.verifying_key().encode(), &edited.canonical(), &sig),
            Err(ApprovalDecodeError::BadSignature)
        );
    }

    #[test]
    fn the_signature_is_domain_separated() {
        let k = key(1);
        let payload = approval().canonical();
        // A bare signature over the payload (no domain) must not verify.
        let bare = k.sign(&payload).encode().to_vec();
        assert_eq!(
            verify_signature(&k.verifying_key().encode(), &payload, &bare),
            Err(ApprovalDecodeError::BadSignature)
        );
    }

    #[test]
    fn a_non_canonical_or_extended_payload_is_refused() {
        let k = key(1);
        let pretty = serde_json::to_vec_pretty(&approval()).unwrap();
        let sig = k.sign(&signed_message_in(HOST_APPROVAL_DOMAIN, &pretty)).encode().to_vec();
        let wire = SignedHostApproval { payload: b64().encode(&pretty), signature: b64().encode(&sig) };
        let encoded = b64().encode(serde_json::to_vec(&wire).unwrap());
        assert_eq!(
            decode_unverified(&encoded).unwrap_err(),
            ApprovalDecodeError::Malformed("payload is not canonical")
        );

        let mut extended: serde_json::Value = serde_json::to_value(approval()).unwrap();
        extended["extra"] = serde_json::json!(1);
        let bytes = serde_json::to_vec(&extended).unwrap();
        let wire = SignedHostApproval { payload: b64().encode(&bytes), signature: b64().encode(&sig) };
        let encoded = b64().encode(serde_json::to_vec(&wire).unwrap());
        assert!(matches!(decode_unverified(&encoded), Err(ApprovalDecodeError::Malformed(_))));
    }
}
