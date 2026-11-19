//! Bots (#1182): a bot is a user scoped to one group, and its hosts are its
//! devices. These are the DS endpoints that create and pair them, relay a host
//! approval, delete them, and let several hosts of one bot agree on who handles
//! a trigger.
//!
//! The DS stores and relays; it verifies no signature here beyond the device
//! signature on the request. The code an admin types is checked by the admin's
//! client against the ephemeral key it reads back, and a host approval is
//! verified by the hosts that act on it.
//!
//! Wire types only; see `pollis-delivery/src/bots.rs`.

use serde::{Deserialize, Serialize};

/// What a pairing request is for. A tagged enum, so a request can carry the
/// bot's account key or name an existing bot, never both and never neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingTarget {
    /// The first host of a new bot. The account key was generated on the host
    /// and never leaves it; this is its ML-DSA-44 public key, base64 (STANDARD).
    NewBot { account_id_pub: String },
    /// Another host for an existing bot.
    ExistingBot { bot_user_id: String },
}

/// `POST /v1/bots/pairing/request` — UNAUTHENTICATED (the host has no
/// credential yet) and rate-limited per IP. The DS assigns the id and the
/// expiry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotPairingRequestBody {
    /// The hash the admin's client derives from the code the host printed,
    /// base64 (STANDARD), 32 bytes. Only a lookup key: the admin's client checks
    /// the code against the ephemeral key it reads back, never against this.
    pub code_lookup: String,
    /// `SHA-256(poll_secret)`, base64 (STANDARD). The host reads the outcome
    /// with the secret; the DS stores only this.
    pub poll_verifier: String,
    /// The host's ephemeral X25519 public key, base64 (STANDARD), 32 bytes. The
    /// code is derived from it, and an added host's account key is wrapped to it.
    pub ephemeral_pub: String,
    /// The host's device id; it becomes a `user_device` row under the bot.
    pub device_id: String,
    /// Shown to the admin. Untrusted display text.
    #[serde(default)]
    pub device_name: Option<String>,
    pub target: PairingTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPairingRequestResponse {
    pub request_id: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// `POST /v1/bots/pairing/lookup` — device-signed by an admin of `group_id`:
/// the open pairing request whose code hashes to `code_lookup`. An
/// `ExistingBot` request is found only through its own bot's group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotPairingLookupBody {
    pub group_id: String,
    /// base64 (STANDARD), 32 bytes.
    pub code_lookup: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPairingLookupResponse {
    /// `None`: no open request with that code for this group.
    #[serde(default)]
    pub request: Option<BotPairingRequestWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPairingRequestWire {
    pub request_id: String,
    /// base64 (STANDARD). The admin's client derives the code from this.
    pub ephemeral_pub: String,
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    pub target: PairingTarget,
    /// Unix seconds.
    pub expires_at: i64,
}

/// `POST /v1/bots/pairing/status` — UNAUTHENTICATED; the poll secret is the
/// credential. What the host awaits after printing its code. Unknown, expired
/// and wrong-secret answer the same 401.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotPairingStatusBody {
    pub request_id: String,
    /// base64 (STANDARD), 32 bytes.
    pub poll_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BotPairingStatus {
    /// Nobody has acted on it yet.
    Pending,
    /// A `NewBot` request: the bot exists, with this host as its device. The
    /// host now publishes its device cert and key packages.
    Created { bot_user_id: String, group_id: String },
    /// An `ExistingBot` request: one of the bot's hosts wrapped the account key
    /// to the request's ephemeral key. base64 (STANDARD) of
    /// `approver_pub || nonce || ciphertext`, the enrollment wrap format.
    Wrapped {
        bot_user_id: String,
        group_id: String,
        wrapped_account_key: String,
        wrapped_by_device_id: String,
    },
}

/// `POST /v1/bots/create` — device-signed by an admin of `group_id`. In one
/// transaction: the bot user, its `bot` row, the host's device and the bot's
/// membership. The admin's client then adds it through the ordinary MLS
/// reconcile, once the host has published its key packages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBotBody {
    pub group_id: String,
    pub request_id: String,
    /// The ephemeral key the admin's client checked the typed code against,
    /// base64 (STANDARD). Must equal the request's, byte for byte.
    pub ephemeral_pub: String,
    /// The bot's display name (`users.preferred_name`).
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BotCreated {
    Ok {
        bot_user_id: String,
        device_id: String,
        group_id: String,
    },
    /// The group already has [`MAX_BOTS_PER_GROUP`] bots.
    LimitReached { limit: u32 },
}

/// Bots one group may have. The DS enforces it at creation; the Bots page shows it.
pub const MAX_BOTS_PER_GROUP: u32 = 25;

/// `POST /v1/read/bots` — device-signed by an admin of `group_id`: the
/// group's bots and their hosts, for the Bots page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListBotsBody {
    pub group_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListBotsResponse {
    pub bots: Vec<BotWire>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotWire {
    pub user_id: String,
    pub username: String,
    #[serde(default)]
    pub preferred_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `None` once the creating admin's account is gone.
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at: String,
    pub hosts: Vec<BotHostWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotHostWire {
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    pub created_at: String,
    pub last_seen: String,
    /// Set once the host published its device cert.
    pub certified: bool,
    #[serde(default)]
    pub revoked_at: Option<String>,
}

/// `POST /v1/bots/delete` — device-signed by an admin of the bot's group.
/// Deletes the bot through the account-deletion path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteBotBody {
    pub bot_user_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `POST /v1/bots/hosts/approve` — device-signed by a current admin of the
/// bot's group: store the admin's signed host approval for an `ExistingBot`
/// request and relay it to the bot's hosts. The DS checks who signed the
/// request, not the approval; the hosts verify it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproveBotHostBody {
    pub bot_user_id: String,
    pub request_id: String,
    /// The signed approval, base64 (STANDARD). Opaque to the DS.
    pub host_approval: String,
}

/// `POST /v1/bots/hosts/pending` — device-signed by one of a bot's hosts: the
/// approved, not yet wrapped requests for another host of that bot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PendingBotHostsBody {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBotHostsResponse {
    pub requests: Vec<PendingBotHostWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBotHostWire {
    pub request_id: String,
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    /// base64 (STANDARD); the host checks it equals the approval's.
    pub ephemeral_pub: String,
    /// base64 (STANDARD).
    pub host_approval: String,
    pub approver_user_id: String,
    pub approver_device_id: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// `POST /v1/bots/hosts/wrap` — device-signed by one of the bot's hosts: the
/// account key wrapped to the new host's ephemeral key. The first wrap wins;
/// the new host becomes a device of the bot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrapBotHostBody {
    pub request_id: String,
    /// base64 (STANDARD) of `approver_pub || nonce || ciphertext`.
    pub wrapped_account_key: String,
}

/// `POST /v1/bots/triggers/claim` — device-signed by a bot's host: claim one
/// trigger for an installation. The first claim wins; a retry by the device
/// that won answers `won` again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimTriggerBody {
    /// The bot's user id, while a bot has one installation.
    pub installation_id: String,
    /// The envelope id, request id, schedule slot or webhook delivery id.
    pub trigger_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimTriggerResponse {
    pub won: bool,
}

// ---------------------------------------------------------------- App state (#2004)

/// `POST /v1/bots/app-state/read` — device-signed by one of the bot's hosts:
/// the sealed App key-value entries for `key_ids`, and the installation's
/// total usage. The DS sees only opaque key ids and ciphertext.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStateReadBody {
    /// The bot's user id, while a bot has one installation.
    pub installation_id: String,
    /// base64 (STANDARD) of 32-byte key ids (HMAC of the App's key under a
    /// key only the bot's hosts hold). At most [`APP_STATE_MAX_KEYS_PER_CALL`].
    pub key_ids: Vec<String>,
}

/// Most key ids in one read, and most writes in one commit.
pub const APP_STATE_MAX_KEYS_PER_CALL: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStateEntry {
    /// base64 (STANDARD), 32 bytes.
    pub key_id: String,
    /// Starts at 1, +1 per write.
    pub version: i64,
    /// base64 (STANDARD) of `nonce(12) || ChaCha20-Poly1305(len(key) u16 BE || key || value)`.
    pub sealed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStateReadResponse {
    /// Only the requested keys that exist.
    pub entries: Vec<AppStateEntry>,
    /// Sum of sealed bytes over the installation's rows.
    pub used_bytes: u64,
    pub rows: u64,
}

/// One write in a commit. `expected_version` is the version the host read
/// (0 when the key was absent); `sealed: None` deletes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStateWrite {
    pub key_id: String,
    pub expected_version: i64,
    pub sealed: Option<String>,
}

/// `POST /v1/bots/app-state/commit` — device-signed by the bot's host that won
/// the claim on `trigger_key`: apply a job's writes, all or none, once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStateCommitBody {
    pub installation_id: String,
    pub trigger_key: String,
    pub writes: Vec<AppStateWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AppStateCommitResponse {
    /// Every write applied.
    Committed,
    /// A key moved since the host read it; nothing applied. Re-run the job.
    Conflict { key_id: String },
    /// This trigger's state was committed before (a retried job); nothing applied.
    AlreadyCommitted,
    /// The writes would take the installation past the DS's ceiling; nothing applied.
    OverQuota { max_bytes: u64 },
}
