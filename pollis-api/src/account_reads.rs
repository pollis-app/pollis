//! Account, device, key-package, enrollment and object READS (#987).
//!
//! The `/v1/read/*` identity family. Three use a credential other than a device signature:
//!
//!   * [`AccountProbeBody`] is **unauthenticated**: it runs before PIN entry, with no signer or session.
//!   * [`EnrollmentRequestBody`] is gated on **possession of `request_id`**: a session would expire first.
//!   * [`RecoveryBlobBody`] accepts an **OTP session only**: the caller has no device yet.
//!
//! Everything else here is device-signed like the rest of the API.

use serde::{Deserialize, Serialize};

/// Cross-signing roots for a batch of users — safety numbers, TOFU pinning, the
/// reconcile pre-pin, and the self-audit's local view.
///
/// Batched: reconcile and the sidebar ask for whole rosters at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeysBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeysResponse {
    /// One entry per user that RESOLVES; missing or NULL keys are absent ("not provisioned, do not pin").
    pub keys: Vec<AccountKeyRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeyRow {
    pub user_id: String,
    /// base64
    pub account_id_pub: String,
    pub identity_version: i64,
}

/// A user's devices, with as much of the cross-signing material as the caller is
/// entitled to.
///
/// For another user, only the public cert-chain columns are returned, never `device_name` or `last_seen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesBody {
    /// Whose devices. `None` → the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Narrow to these device ids. Empty → every device of the user.
    #[serde(default)]
    pub device_ids: Vec<String>,
    /// Include tombstoned rows; cert verification needs them ("revoked" differs from "absent").
    #[serde(default)]
    pub include_revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceRow>,
}

/// One `user_device` row.
///
/// All `Option` (nullable mid-enrollment); `device_name` and `last_seen` are `None` for non-owners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
    /// base64
    #[serde(default)]
    pub device_cert: Option<String>,
    #[serde(default)]
    pub cert_issued_at: Option<String>,
    #[serde(default)]
    pub cert_identity_version: Option<i64>,
    /// base64
    #[serde(default)]
    pub mls_signature_pub: Option<String>,
    /// base64
    #[serde(default)]
    pub mls_signature_pub_pq: Option<String>,
}

/// Key-package discovery: how many of MY packages are left, and which of a
/// roster's devices have one to claim.
///
/// One endpoint: the same table, and reconcile wants both halves together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPackagesBody {
    /// Count MY unclaimed packages in this suite, for the replenish shortfall.
    /// `None` → do not count.
    #[serde(default)]
    pub count_for_ciphersuite: Option<i64>,
    /// List `(user_id, device_id)` pairs among these users that have at least
    /// one unclaimed package in `pairs_ciphersuite`.
    #[serde(default)]
    pub pairs_for_user_ids: Vec<String>,
    #[serde(default)]
    pub pairs_ciphersuite: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPackagesResponse {
    /// Unclaimed packages the authenticated device still has in the requested
    /// suite. `None` when the caller did not ask.
    #[serde(default)]
    pub unclaimed: Option<i64>,
    /// Does the device hold a last-resort package (#1571)? `None` from an older DS; never counted in `unclaimed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_last_resort: Option<bool>,
    /// `(user_id, device_id)` pairs with a claimable package.
    ///
    /// The DS errors rather than truncating: a silently short list keeps a device out of a group.
    #[serde(default)]
    pub pairs: Vec<DevicePair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePair {
    pub user_id: String,
    pub device_id: String,
}

/// Every non-revoked `(user_id, device_id)` still registered for a roster.
///
/// Reconcile drops leaves whose device was revoked; pairs only, no cert material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevicesBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevicesResponse {
    pub pairs: Vec<DevicePair>,
}

/// Every roster user's cross-signing root plus ALL of their `user_device` rows
/// (revoked ones included — "revoked" is a verdict, "absent" a retry).
///
/// The committer's leaf cross-signing inputs, batched over the roster; the verdict stays client-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterIdentitiesBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterIdentitiesResponse {
    pub identities: Vec<crate::reads::AddedIdentity>,
}

/// Poll one device-enrollment request, or list the caller's pending ones.
///
/// `request_id` is the credential (the device has no key, and its session expires first); the blob is sealed to its ephemeral key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequestBody {
    /// The capability. A 128-bit ULID minted by the requesting device.
    pub request_id: String,
    /// Set by an approving sibling to get the approval columns; served only to the request's owner.
    #[serde(default)]
    pub want_approval_fields: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequestResponse {
    /// `None` → no such request.
    #[serde(default)]
    pub request: Option<EnrollmentRequestRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequestRow {
    pub id: String,
    pub user_id: String,
    pub new_device_id: String,
    pub status: String,
    pub expires_at: String,
    pub created_at: String,
    /// base64, present once the request is approved.
    #[serde(default)]
    pub wrapped_account_key: Option<String>,
    /// base64. Approval-side only.
    #[serde(default)]
    pub new_device_ephemeral_pub: Option<String>,
    /// Approval-side only.
    #[serde(default)]
    pub verification_code: Option<String>,
}

/// List the authenticated user's open enrollment requests (device-signed; it enumerates).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEnrollmentsBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEnrollmentsResponse {
    pub requests: Vec<EnrollmentRequestRow>,
}

/// The Secret-Key-wrapped account identity, for recovery on a device with
/// nothing.
///
/// OTP session only: the caller has no device. Rate-limited, and each fetch records a security event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryBlobBody {
    /// Must equal the session's user; a mismatch is a 403.
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryBlobResponse {
    /// `None` → no recovery blob on file for this account.
    #[serde(default)]
    pub blob: Option<RecoveryBlob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryBlob {
    /// base64
    pub salt: String,
    /// base64
    pub nonce: String,
    /// base64
    pub wrapped_key: String,
}

/// The caller's own account row: identity version, email, username.
///
/// Self-scoped by construction (it carries `email`); the DS ignores any id in the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatusBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatusResponse {
    /// `None` → the account row is gone (deleted elsewhere).
    #[serde(default)]
    pub account: Option<AccountStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStatus {
    pub user_id: String,
    pub identity_version: i64,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    /// base64. `None` before the first identity is established — which is
    /// precisely what "this device needs enrollment" is derived from.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}

/// Does this account exist, and does it have an identity yet?
///
/// Unauthenticated and IP-rate-limited: it runs before PIN entry, with no signer or session (#987 §6.5).
///
/// It discloses less than `verify-otp`: `user_id` is an unguessable ULID the caller already holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProbeBody {
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProbeResponse {
    /// The account's current `identity_version`, when it has an identity.
    ///
    /// Here because `ensure_device_cert` needs it before the device can sign; it is public in every cert.
    #[serde(default)]
    pub identity_version: Option<i64>,
    /// The `users` row exists. `false` means the locally cached account is stale
    /// and should be dropped.
    pub exists: bool,
    /// `users.account_id_pub` is non-NULL. `false` on an existing account means
    /// this install must go through enrollment before it can unlock.
    pub has_identity: bool,
}

/// The caller's own security-event log, most recent first, one keyset page at a
/// time (#1548).
///
/// `before` and `kinds` are skipped when unset, so an old body gets the old answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventsBody {
    #[serde(default)]
    pub user_id: Option<String>,
    /// Clamped server-side to `1..=500`.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Return only rows strictly older than this `(created_at, id)` — the
    /// previous page's `next_cursor`. `None` = start at the newest row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<SecurityEventCursor>,
    /// Return only these `kind`s. `None` or empty = every kind. At most
    /// [`SECURITY_EVENTS_MAX_KINDS`]; a longer list is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
}

/// The most `kinds` one [`SecurityEventsBody`] may name (bounds the parameter count).
pub const SECURITY_EVENTS_MAX_KINDS: usize = 32;

/// A keyset position in the security-event log: the `(created_at, id)` of the
/// last row a page returned. `id` is the tiebreak, so rows written in the same
/// second are each served exactly once across a page boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityEventCursor {
    pub created_at: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventsResponse {
    pub events: Vec<SecurityEventRow>,
    /// Where the next page starts, or `None` at the oldest matching row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<SecurityEventCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventRow {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub device_id: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub metadata: Option<String>,
}

/// Custom emoji: the usable set (every group the caller is in) and single-object
/// resolution for rendering.
///
/// Enumeration is members-only group metadata; resolving a hash you were handed is ungated (#848).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmojiReadBody {
    /// Every emoji usable by the caller — the picker's source AND the send
    /// permission rule, which is why they are one query.
    #[serde(default)]
    pub want_usable: bool,
    /// Resolve these content hashes to their stored object metadata. Not
    /// membership-gated, by design.
    #[serde(default)]
    pub content_hashes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmojiReadResponse {
    #[serde(default)]
    pub usable: Vec<crate::directory::CustomEmojiWire>,
    #[serde(default)]
    pub objects: Vec<EmojiObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmojiObject {
    pub content_hash: String,
    pub r2_key: String,
    pub content_type: String,
}

/// Content-addressed dedup probe: is this blob already stored?
///
/// One endpoint for both object tables; the caller names the table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectExistsBody {
    pub content_hash: String,
    pub kind: ObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Attachment,
    Emoji,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectExistsResponse {
    pub exists: bool,
}

/// This account's synced read-cursor blob, so a device can merge the other
/// devices' read positions into its own (#844).
///
/// Device-signed and bound to the authenticated user, so size and change rate do not leak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadCursorsBody {
    /// Must equal the authenticated user; a mismatch is a 403.
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadCursorsResponse {
    /// `None` → this account has never pushed a cursor blob. A first-run device
    /// gets this and simply has nothing to merge; it is not an error.
    #[serde(default)]
    pub cursors: Option<EncryptedReadCursors>,
}

/// The stored blob, exactly as the DS holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedReadCursors {
    /// base64 — the 12-byte AES-256-GCM nonce.
    pub nonce: String,
    /// base64 — AES-256-GCM ciphertext + tag.
    pub blob: String,
}
