//! Device and key-package lifecycle: publish/claim/replenish key packages,
//! re-sign device certs, register push tokens.
//!
//! Wire types only; see `pollis-delivery` for handling.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct PublishKeyPackagesBody {
    /// The publishing device. Must be one of the actor's own devices; the rows
    /// are written with `user_id = actor` regardless, so this only labels which
    /// device produced them.
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    /// No-auth fallback for the actor; when signed it must equal the
    /// authenticated user.
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
    /// The user whose published key package is being claimed (the device being
    /// added to the MLS group). NOT the caller — claiming a *peer's* package is
    /// how you add them, so this is intentionally not bound to the signer.
    pub target_user_id: String,
    /// Narrow the claim to one of the target's devices; absent → any unclaimed package.
    #[serde(default)]
    pub target_device_id: Option<String>,
    /// Narrow the claim to packages published in this MLS ciphersuite. Optional:
    /// absent means [`CIPHERSUITE_PQ`], the only suite in use.
    ///
    /// A suite with no unclaimed package yields `NoKeyPackage`, never a wrong-suite package.
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
    /// RFC3339 timestamp the client stamped. Plain text (never parsed here).
    pub updated_at: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// The MLS code point of `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`
/// — the post-quantum suite, and since #669 the only suite Pollis uses. It is
/// also the default for any request that does not name one.
///
/// The DS only ever uses this as a routing/label value; it never parses the
/// opaque `key_package` blob to confirm the suite.
///
/// Provisional draft code point (#668); must move together with `CS_PQ` in `pollis-core`.
pub const CIPHERSUITE_PQ: i64 = 0x0052;

/// One published key package: its hex hash-ref and the TLS-serialized
/// `KeyPackage` bytes, base64 (STANDARD) since they are binary.
#[derive(Serialize, Deserialize)]
pub struct KeyPackageEntry {
    pub ref_hash: String,
    /// base64 (STANDARD) of the TLS-serialized `KeyPackage`.
    pub key_package: String,
    /// MLS ciphersuite code point this package was built with. Optional: absent
    /// means [`CIPHERSUITE_PQ`], the only suite in use.
    ///
    /// Client-supplied routing hint only; MLS rejects a mismatch at validation.
    #[serde(default)]
    pub ciphersuite: Option<i64>,
    /// This is the device's LAST-RESORT package in its suite (#1571, RFC 9420
    /// §16.8): handed out only when the device has no unclaimed regular package
    /// left, and never consumed, so a registered device can always be added.
    ///
    /// Additive: omitted on the wire when false, so old clients and DSes are unaffected.
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
    /// Unix seconds, decimal ASCII (stored as TEXT for lossless round-trip).
    pub cert_issued_at: String,
    pub cert_identity_version: i64,
}

impl KeyPackageEntry {
    /// The suite to persist for this package, defaulting to the current one.
    pub fn suite(&self) -> i64 {
        self.ciphersuite.unwrap_or(CIPHERSUITE_PQ)
    }
}

/// `POST /v1/key-packages/claim` — the claimed KeyPackage.
///
/// A 404 (nothing to claim) carries no body: the add path's skip signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimKeyPackageResponse {
    /// The claimed row's hash-ref, so a caller can correlate the claim with the
    /// `mls_key_package` row it consumed.
    pub ref_hash: String,
    /// TLS-serialized MLS KeyPackage, base64 (STANDARD).
    pub key_package: String,
}

/// Resolve an opaque push handle to the conversation it was minted for (#1122).
///
/// The push payload carries only `h`, so providers never see the conversation.
///
/// Scoped to the caller: someone else's handle answers not found, never forbidden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvePushHandleBody {
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvePushHandleResponse {
    /// `None` when the handle is unknown, expired, swept, or not this user's.
    /// The caller degrades to opening the app rather than the conversation.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// `"dm"` or `"channel"`. Moved out of the payload with the id — it is
    /// low-entropy, but it still told the provider DM-vs-channel every time.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Most conversations one dismiss request may name. A client sends the
/// conversations whose read cursor moved in one short debounce window, which
/// is a handful; anything past this is refused as a bad request.
pub const DISMISS_MAX_CONVERSATIONS: usize = 64;

/// Most handles one `/v1/push/dismissed` query may ask about — a phone's tray,
/// which Android caps at about fifty notifications per app.
pub const DISMISSED_QUERY_MAX_HANDLES: usize = 100;

/// One conversation a client read, and how long ago.
///
/// An age, not a timestamp, so the clocks never have to agree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissEntry {
    pub conversation_id: String,
    /// Whole seconds since the cursor moved, rounded UP (so the DS's cutoff can
    /// only fall before the read, never after it).
    pub read_secs_ago: u32,
}

/// "I read these conversations": clear their notifications on every device of
/// mine (#1508).
///
/// Device-signed and self-scoped; the silent push carries only handles, never conversation ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissPushBody {
    pub user_id: String,
    pub conversations: Vec<DismissEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DismissPushResponse {
    /// How many notifications were newly marked dismissed. Zero when there was
    /// nothing outstanding — the common case, which sends no push at all.
    #[serde(default)]
    pub dismissed: u32,
}

/// "Which of these notifications in my tray have been read since?" (#1508)
///
/// Catch-up for a missed silent push, asked on foreground; scoped to the signer.
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
