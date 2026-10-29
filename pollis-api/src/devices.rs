//! Device and key-package lifecycle: publish/claim/replenish key packages,
//! re-sign device certs, register push tokens.
//!
//! Wire types only; see the matching `pollis-delivery` module for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct PublishKeyPackagesBody {
    /// The publishing device, one of the actor's own; rows are written with
    /// `user_id = actor` regardless.
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    /// No-auth fallback; when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ReplenishKeyPackagesBody {
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ClaimKeyPackageBody {
    /// The user whose key package is claimed (the one being added). Not the
    /// caller, so intentionally not bound to the signer.
    pub target_user_id: String,
    /// Narrow the claim to one of the target's devices (the `reconcile` add path
    /// always does); absent → any of the target's unclaimed packages.
    #[serde(default)]
    pub target_device_id: Option<String>,
    /// Narrow the claim to this ciphersuite; absent → [`CIPHERSUITE_PQ`]. A suite
    /// the target has no package in yields `NoKeyPackage`, never a package in the
    /// wrong suite, so a stale retired-suite package is never handed out.
    #[serde(default)]
    pub ciphersuite: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct ResignDeviceCertsBody {
    pub certs: Vec<ResignedCert>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PushTokenBody {
    pub token: String,
    pub platform: String,
    /// RFC3339 timestamp the client stamped; never parsed.
    pub updated_at: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// The MLS code point of `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`,
/// the only suite Pollis uses (#668, #669) and the default when none is named.
/// The DS uses it only as a label, never parsing the blob.
///
/// A provisional `draft-ietf-mls-pq-ciphersuites` code point: it and `CS_PQ` in
/// `pollis-core` must move together, since the DS matches exactly what clients
/// tag. Pools under an old point are never claimed and age out.
pub const CIPHERSUITE_PQ: i64 = 0x0052;

/// One published key package: its hex hash-ref and the `KeyPackage` bytes.
#[derive(Serialize, Deserialize)]
pub struct KeyPackageEntry {
    pub ref_hash: String,
    /// base64 (STANDARD) of the TLS-serialized `KeyPackage`.
    pub key_package: String,
    /// Ciphersuite code point; absent → [`CIPHERSUITE_PQ`]. Client-supplied, so
    /// a routing hint only: MLS rejects a mismatch with the real suite inside
    /// the blob, so mistagging at worst fails an add.
    #[serde(default)]
    pub ciphersuite: Option<i64>,
    /// The device's last-resort package in its suite (#1571, RFC 9420 §16.8):
    /// handed out only when no regular package is left, and never consumed.
    /// Omitted when false, so older peers on either side see a regular package.
    #[serde(default, skip_serializing_if = "is_false")]
    pub last_resort: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One re-signed cross-signing cert for a device of the actor's account.
#[derive(Serialize, Deserialize)]
pub struct ResignedCert {
    pub device_id: String,
    /// base64 (STANDARD) of the signed cert bytes.
    pub device_cert: String,
    /// Unix seconds, decimal ASCII (stored as TEXT).
    pub cert_issued_at: String,
    pub cert_identity_version: i64,
}

impl KeyPackageEntry {
    pub fn suite(&self) -> i64 {
        self.ciphersuite.unwrap_or(CIPHERSUITE_PQ)
    }
}

/// `POST /v1/key-packages/claim`: the claimed KeyPackage. A 404 with no body
/// (no unclaimed package) is the add path's "skip this device" signal, not this
/// type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimKeyPackageResponse {
    /// The claimed row's hash-ref, to correlate with the consumed row.
    pub ref_hash: String,
    /// TLS-serialized MLS KeyPackage, base64 (STANDARD).
    pub key_package: String,
}

/// Resolve an opaque push handle to its conversation (#1122): pushes carry only
/// `h`, so providers never see which conversation. Scoped to the signer; someone
/// else's handle answers not found, never forbidden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvePushHandleBody {
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvePushHandleResponse {
    /// `None` when unknown, expired, swept or not this user's; the caller then
    /// just opens the app.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// `"dm"` or `"channel"`, kept out of the payload so providers don't learn it.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Most conversations one dismiss request may name (one debounce window's worth).
pub const DISMISS_MAX_CONVERSATIONS: usize = 64;

/// Most handles one `/v1/push/dismissed` query may ask about (Android caps a
/// tray at about fifty per app).
pub const DISMISSED_QUERY_MAX_HANDLES: usize = 100;

/// One conversation a client read, and how long ago: an age, so the clocks never
/// have to agree. The DS clears only notifications it minted before then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissEntry {
    pub conversation_id: String,
    /// Whole seconds since the cursor moved, rounded up, so the cutoff never
    /// falls after the read.
    pub read_secs_ago: u32,
}

/// "I read these conversations": clear their notifications on all my devices
/// (#1508). `user_id` must equal the signer. The DS marks the `push_handle` rows
/// and sends one silent push carrying only the handles to clear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissPushBody {
    pub user_id: String,
    pub conversations: Vec<DismissEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissPushResponse {
    /// Notifications newly marked dismissed; zero (the common case) sends no push.
    #[serde(default)]
    pub dismissed: u32,
}

/// "Which of these tray notifications have been read since?" (#1508), for a
/// device that missed the silent push; asked on foreground and unlock. A handle
/// that is not the signer's is simply not returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissedPushHandlesBody {
    pub user_id: String,
    pub handles: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissedPushHandlesResponse {
    /// The subset of the asked handles that are this user's and were read.
    #[serde(default)]
    pub dismissed: Vec<String>,
}
