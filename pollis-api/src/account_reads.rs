//! Account, device, key-package, enrollment and object reads: the `/v1/read/*`
//! family (#987). Device-signed, except three with documented reasons:
//! [`AccountProbeBody`] (unauthenticated), [`EnrollmentRequestBody`]
//! (`request_id` is the capability) and [`RecoveryBlobBody`] (OTP session only).

use serde::{Deserialize, Serialize};

/// Cross-signing roots for a batch of users (safety numbers, TOFU pinning,
/// reconcile, self-audit). Batched because every caller is roster-shaped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeysBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeysResponse {
    /// One entry per user that resolves. A missing row or NULL `account_id_pub`
    /// is absent, which TOFU reads as "not provisioned yet, do not pin".
    pub keys: Vec<AccountKeyRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeyRow {
    pub user_id: String,
    /// base64
    pub account_id_pub: String,
    pub identity_version: i64,
}

/// A user's devices. For another user's devices only the cert chain (already
/// public to group members) is returned, never `device_name` or `last_seen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesBody {
    /// Whose devices. `None` → the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Narrow to these device ids. Empty → every device of the user.
    #[serde(default)]
    pub device_ids: Vec<String>,
    /// Include tombstoned rows: cert verification needs them, because "revoked"
    /// and "absent" are different verdicts.
    #[serde(default)]
    pub include_revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceRow>,
}

/// One `user_device` row. NULL cert columns on a live row mean "cert publish
/// not landed yet" (retry); non-NULL `revoked_at` is a verdict. `device_name`
/// and `last_seen` are `None` for anyone but the owner.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPackagesBody {
    /// Count MY unclaimed packages in this suite. `None` → do not count.
    #[serde(default)]
    pub count_for_ciphersuite: Option<i64>,
    /// Users whose `(user_id, device_id)` pairs with an unclaimed package in
    /// `pairs_ciphersuite` to list.
    #[serde(default)]
    pub pairs_for_user_ids: Vec<String>,
    #[serde(default)]
    pub pairs_ciphersuite: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPackagesResponse {
    /// The caller's unclaimed packages in the requested suite; `None` if not asked.
    #[serde(default)]
    pub unclaimed: Option<i64>,
    /// Whether the caller holds a last-resort package in the counted suite
    /// (#1571); never counted in `unclaimed`. `None` from a DS that predates
    /// last-resort packages, so the client must not publish one (it would be
    /// stored as a regular package).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_last_resort: Option<bool>,
    /// `(user_id, device_id)` pairs with a claimable package. The DS errors
    /// rather than truncating: a silently short list keeps a device out of a
    /// group.
    #[serde(default)]
    pub pairs: Vec<DevicePair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePair {
    pub user_id: String,
    pub device_id: String,
}

/// Every non-revoked `(user_id, device_id)` registered for a roster, so
/// reconcile can drop leaves of revoked devices whose user is still a member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevicesBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevicesResponse {
    pub pairs: Vec<DevicePair>,
}

/// Every roster user's cross-signing root plus all their `user_device` rows,
/// revoked included ("revoked" is a verdict, "absent" a retry).
///
/// Inputs to the committer's leaf cross-signing check only: the verdict is a
/// signature check rooted in the user's identity key, which the DS is not
/// trusted to evaluate.
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
/// `request_id` is the credential: the polling device has no signing key, and
/// its OTP session (minted before the request, same 600s TTL) is guaranteed to
/// expire first. Safe because `wrapped_account_key` is sealed to the
/// requester's in-memory ephemeral X25519 key; a holder of the id learns only a
/// status and a blob they cannot open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequestBody {
    /// The capability: a 128-bit ULID minted by the requesting device.
    pub request_id: String,
    /// Set by an approving sibling that needs `new_device_ephemeral_pub` and
    /// `verification_code`. Served only to a signed caller who owns the
    /// request's user, never to a bare capability holder.
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

/// The authenticated user's open enrollment requests (existing-device side, in
/// case the inbox push was missed). Device-signed because it enumerates.
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
/// OTP session only: the caller has no device by definition. The DS
/// rate-limits it and records a security event per fetch, since a fetch is the
/// signal an owner wants to see if it was not them.
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

/// The caller's own account row. Self-scoped (any id in the body is ignored)
/// because it carries `email`.
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
    /// base64. `None` before the first identity is established, which is how
    /// "this device needs enrollment" is derived.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}

/// Does this account exist, and does it have an identity yet?
///
/// Unauthenticated and IP-rate-limited (#987 §6.5): it runs at every launch
/// before PIN entry, when no device signer, `device_id` or session exists, and
/// it gates the pre-unlock UI. Discloses less than `verify-otp`, which answers
/// `has_identity` for a guessable email; `user_id` is an unguessable ULID from
/// the caller's own `accounts.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProbeBody {
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProbeResponse {
    /// The account's current `identity_version`, when it has an identity.
    ///
    /// Here rather than on `account-status` because `ensure_device_cert` must
    /// stamp it before the device's `mls_signature_pub` exists, so neither a
    /// device signature nor a session (which a slow sibling approval outlives)
    /// can authenticate the read. Not a disclosure: every cert and
    /// `account-keys` already expose it.
    #[serde(default)]
    pub identity_version: Option<i64>,
    /// `false` → the locally cached account is stale and should be dropped.
    pub exists: bool,
    /// `false` on an existing account → this install must enroll before it can
    /// unlock.
    pub has_identity: bool,
}

/// The caller's own security-event log, newest first, one keyset page at a time
/// (#1548). A body without `before`/`kinds` gets the newest `limit` rows of
/// every kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventsBody {
    #[serde(default)]
    pub user_id: Option<String>,
    /// Clamped server-side to `1..=500`.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Return only rows strictly older than this cursor (the previous page's
    /// `next_cursor`). `None` = start at the newest row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<SecurityEventCursor>,
    /// Return only these `kind`s; `None` or empty = every kind. At most
    /// [`SECURITY_EVENTS_MAX_KINDS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
}

/// The most `kinds` one [`SecurityEventsBody`] may name; bounds the
/// bound-parameter count.
pub const SECURITY_EVENTS_MAX_KINDS: usize = 32;

/// A keyset position: the `(created_at, id)` of a page's last row. `id` breaks
/// ties so same-second rows are served exactly once across a page boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityEventCursor {
    pub created_at: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventsResponse {
    pub events: Vec<SecurityEventRow>,
    /// Where the next page starts; `None` when this page reached the oldest
    /// matching row.
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

/// Custom emoji: the usable set and single-object resolution.
///
/// Two rules (#848): enumerating a group's emoji is members-only group
/// metadata; resolving a hash you were already handed is part of reading a
/// message and deliberately ungated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmojiReadBody {
    /// Every emoji usable by the caller: the picker's source and the send
    /// permission rule.
    #[serde(default)]
    pub want_usable: bool,
    /// Resolve these content hashes to object metadata. Not membership-gated, by
    /// design.
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

/// Content-addressed dedup probe: is this blob already stored in `kind`'s table?
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

/// This account's synced read-cursor blob, so a device can merge other
/// devices' read positions (#844). Bound to the authenticated user: the blob is
/// undecryptable, but serving it to anyone would leak its size and change rate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadCursorsBody {
    /// Must equal the authenticated user; a mismatch is a 403.
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadCursorsResponse {
    /// `None` → this account has never pushed a cursor blob; not an error.
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
