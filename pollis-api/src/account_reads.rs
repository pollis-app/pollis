//! Account, device, key-package, enrollment and object READS (#987).
//!
//! The `/v1/read/*` family: everything about *identities and their material*
//! that the client used to `SELECT` for itself. Three of these carry a credential
//! that is not a device signature, and each has a reason:
//!
//!   * [`AccountProbeBody`] is **unauthenticated**, because it runs at every app
//!     launch BEFORE PIN entry — the local database is closed, so `ds_post`
//!     cannot load the device signer, and no OTP has run either, so there is no
//!     session. See §6.5 of #987 and the doc on the type.
//!   * [`EnrollmentRequestBody`] is gated on **possession of `request_id`**, not
//!     on a session, because a session-gated enrollment poll is *guaranteed* to
//!     401 before the request it polls expires: the DS session TTL and the
//!     enrollment TTL are both 600s, and the session is minted at `verify-otp`,
//!     strictly BEFORE the request is created.
//!   * [`RecoveryBlobBody`] accepts an **OTP session only** — never a plain
//!     request and never a device signature, because the caller by definition has
//!     no device yet and the blob is the account's last line of recovery.
//!
//! Everything else here is device-signed like the rest of the API.

use serde::{Deserialize, Serialize};

// ── POST /v1/read/account-keys ───────────────────────────────────────────────

/// Cross-signing roots for a batch of users — safety numbers, TOFU pinning, the
/// reconcile pre-pin, and the self-audit's local view.
///
/// Batched because the sites that use it are batch-shaped: a whole group roster
/// on reconcile, the whole pinned-contact list when the sidebar opens. The
/// per-user version of this cost one round trip per contact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeysBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeysResponse {
    /// One entry per user that RESOLVES. A requested id with no row, or with a
    /// NULL `account_id_pub`, is absent — which is what the direct read did, and
    /// what the TOFU logic reads as "not provisioned yet, do not pin".
    pub keys: Vec<AccountKeyRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountKeyRow {
    pub user_id: String,
    /// base64
    pub account_id_pub: String,
    pub identity_version: i64,
}

// ── POST /v1/read/devices ────────────────────────────────────────────────────

/// A user's devices, with as much of the cross-signing material as the caller is
/// entitled to.
///
/// Serves the device-list UI, the "is this device still registered" check, the
/// stale-cert sweep and the avatar-free roster lookups. Callers ask for one
/// user's devices; asking for ANOTHER user's returns only the columns that are
/// already public to any group member (the cert chain), never `device_name` or
/// `last_seen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesBody {
    /// Whose devices. `None` → the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Narrow to these device ids. Empty → every device of the user.
    #[serde(default)]
    pub device_ids: Vec<String>,
    /// Include tombstoned (`revoked_at IS NOT NULL`) rows. The device list wants
    /// them hidden; cert verification needs the tombstone, because "revoked" and
    /// "absent" are different verdicts.
    #[serde(default)]
    pub include_revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceRow>,
}

/// One `user_device` row.
///
/// Every column is `Option` because every one is genuinely nullable
/// mid-enrollment, and the client distinguishes the states: NULL cert columns on
/// a live row mean "cert publish has not landed yet" (retry), a non-NULL
/// `revoked_at` is a verdict. `device_name` and `last_seen` are `None` for
/// anyone but the owner.
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

// ── POST /v1/read/key-packages ───────────────────────────────────────────────

/// Key-package discovery: how many of MY packages are left, and which of a
/// roster's devices have one to claim.
///
/// The two halves share an endpoint because they are the same table and the
/// reconcile path wants the second immediately after the roster read it already
/// paid for.
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
    /// `(user_id, device_id)` pairs with a claimable package.
    ///
    /// A short list here is indistinguishable from "these devices have no
    /// package", which is why the DS answers an ERROR rather than a truncated
    /// list if any chunk of the query fails — a silently short roster is how a
    /// device quietly stays out of a group.
    #[serde(default)]
    pub pairs: Vec<DevicePair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePair {
    pub user_id: String,
    pub device_id: String,
}

// ── POST /v1/read/registered-devices ─────────────────────────────────────────

/// Every non-revoked `(user_id, device_id)` still registered for a roster.
///
/// Reconcile uses it to drop leaves whose device row was revoked while the user
/// remains a member. Separate from [`DevicesBody`] because it is roster-wide and
/// returns no cert material — only the pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevicesBody {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredDevicesResponse {
    pub pairs: Vec<DevicePair>,
}

// ── POST /v1/read/enrollment ─────────────────────────────────────────────────

/// Poll one device-enrollment request, or list the caller's pending ones.
///
/// # Why `request_id` is the credential
///
/// The polling device has no signing key (that is what it is being enrolled to
/// get) and cannot use its OTP session either: the session is minted at
/// `verify-otp`, strictly BEFORE the enrollment request is created, and both
/// TTLs are 600 seconds — so a session-gated poll is *guaranteed* to 401 before
/// the request it is polling expires. Verified, not assumed.
///
/// Possession of `request_id` is cryptographically safe as the gate:
/// `wrapped_account_key` is sealed to the requesting device's ephemeral X25519
/// public key, whose private half never leaves that process's memory. A holder of
/// the id learns a status and a blob they cannot open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRequestBody {
    /// The capability. A 128-bit ULID minted by the requesting device.
    pub request_id: String,
    /// Set by an APPROVING sibling — a fully enrolled, device-signing client
    /// that needs the columns the approval flow validates against
    /// (`new_device_ephemeral_pub`, `verification_code`). Served only to a
    /// signed caller who owns the request's user; a bare capability holder
    /// never receives them.
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

/// List the authenticated user's still-open enrollment requests — the
/// existing-device side, called at login in case the inbox push was missed.
/// Device-signed; this is not capability-gated because it enumerates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEnrollmentsBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEnrollmentsResponse {
    pub requests: Vec<EnrollmentRequestRow>,
}

// ── POST /v1/read/recovery-blob ──────────────────────────────────────────────

/// The Secret-Key-wrapped account identity, for recovery on a device with
/// nothing.
///
/// OTP session only — never plain, never device-signed. The caller has no device
/// (that is the situation), and the blob is the account's last line of recovery,
/// so the credential has to be the email OTP they just proved. The DS
/// rate-limits this hard and records a security event per fetch: the blob is
/// useless without the Secret Key, but a *fetch* is exactly the signal an account
/// owner wants to see if it was not them.
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

// ── POST /v1/read/account-status ─────────────────────────────────────────────

/// The caller's own account row: identity version, email, username.
///
/// Self-scoped by construction — the DS answers for the authenticated user and
/// ignores any id in the body — because `email` is on it. Serves cert signing
/// (which needs `identity_version` to stamp), the identity-rotation CAS
/// expectation, and the reset flow's email confirmation.
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

// ── POST /v1/auth/account-probe ──────────────────────────────────────────────

/// Does this account exist, and does it have an identity yet?
///
/// # The bootstrap dead end (#987 §6.5)
///
/// `get_session` runs at EVERY app launch, before PIN entry. At that moment the
/// local SQLCipher database is closed, so `ds_post` cannot load the ML-DSA-44
/// device signer and errors with *"not signed in for DS request signing"*;
/// `device_id` is not set either; and no OTP has run, so there is no session
/// bearer. There is no credential available, and the answer gates the
/// PRE-UNLOCK UI — a device that needs enrollment has no PIN to unlock with — so
/// it cannot be deferred until after unlock.
///
/// Hence: unauthenticated, and rate-limited by IP.
///
/// The disclosure is strictly smaller than one we already ship. `user_id` is a
/// 128-bit ULID that the caller read out of its OWN `accounts.json` — it is not
/// guessable and not an enumeration surface — while `verify-otp` already returns
/// `has_identity` for an *email address*, which is guessable. Answering here for
/// an id the caller already holds tells an attacker nothing they could not learn
/// more easily elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProbeBody {
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProbeResponse {
    /// The account's current `identity_version`, when it has an identity.
    ///
    /// Here rather than on `account-status` because of the BOOTSTRAP PIVOT:
    /// `ensure_device_cert` must stamp this version into the cert it is about to
    /// sign, and it runs on a device whose `user_device.mls_signature_pub` is
    /// still NULL — the column the DS reads to authenticate a signature. The
    /// write that establishes the credential cannot be authenticated by that
    /// credential, and neither can the read that feeds it. A session does not
    /// close the gap either: sibling-approval enrollment publishes its cert on
    /// cert-validity ALONE precisely because a slow human approval outlives the
    /// session TTL.
    ///
    /// It is not a disclosure. Every device cert carries `cert_identity_version`
    /// in the clear, `POST /v1/read/account-keys` returns it for any user id to
    /// any authenticated caller, and it is a small counter of how many times the
    /// account has been reset. `email` and `username`, which are NOT public in
    /// that sense, stay on the authenticated `account-status` read.
    #[serde(default)]
    pub identity_version: Option<i64>,
    /// The `users` row exists. `false` means the locally cached account is stale
    /// and should be dropped.
    pub exists: bool,
    /// `users.account_id_pub` is non-NULL. `false` on an existing account means
    /// this install must go through enrollment before it can unlock.
    pub has_identity: bool,
}

// ── POST /v1/read/security-events ────────────────────────────────────────────

/// The caller's own security-event log, most recent first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventsBody {
    #[serde(default)]
    pub user_id: Option<String>,
    /// Clamped server-side to `1..=500`, matching the client's previous clamp,
    /// so a body cannot ask for the whole table.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEventsResponse {
    pub events: Vec<SecurityEventRow>,
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

// ── POST /v1/read/emoji ──────────────────────────────────────────────────────

/// Custom emoji: the usable set (every group the caller is in) and single-object
/// resolution for rendering.
///
/// The two are one endpoint but NOT one rule, and the distinction is #848's:
/// enumerating a group's emoji is group metadata (shortcodes leak the way
/// channel names do, `created_by` names members) and is members-only, while
/// RESOLVING one hash you were already handed is part of reading a message you
/// can already read, and is deliberately ungated.
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

// ── POST /v1/read/objects ────────────────────────────────────────────────────

/// Content-addressed dedup probe: is this blob already stored?
///
/// One endpoint for both object tables because the question and the answer are
/// the same shape; the caller says which table it means.
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
