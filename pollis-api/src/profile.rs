//! Profile, preferences, blocks and DM conversations.
//!
//! Wire types only; see `pollis-delivery` for handling.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct UpdateProfileBody {
    /// The profile being edited; when signed it must equal the authenticated
    /// user (you can only edit your OWN profile).
    pub user_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub preferred_name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SavePreferencesBody {
    /// The owner of the preferences; when signed it must equal the authenticated
    /// user (you can only edit your OWN preferences).
    pub user_id: String,
    pub preferences: String,
}

#[derive(Serialize, Deserialize)]
pub struct BlockBody {
    /// The user doing the blocking; when signed it must equal the authenticated
    /// user (you manage only your OWN block list).
    pub blocker_id: String,
    pub blocked_id: String,
}

/// `POST /v1/dm/create` — the DM, whether it was created now or already existed
/// (#987).
///
/// The dedupe happens inside the write, so concurrent creates cannot make two DMs per pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CreatedDm {
    Ok {
        id: String,
        created_by: String,
        created_at: String,
        members: Vec<DmMember>,
        /// `true` when an existing 2-person DM was returned instead of a new one
        /// being created. The caller skips MLS group creation in that case — the
        /// group already exists.
        existing: bool,
    },
    /// A proposed pairing is blocked in either direction. One generic refusal for
    /// every direction, so neither side can infer why their DM failed.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmMember {
    pub user_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub accepted_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateDmBody {
    /// The new channel id (a ULID the client generated).
    pub id: String,
    /// The creator; when signed it must equal the authenticated user.
    pub creator_id: String,
    /// Every participant (may or may not include the creator — the creator is
    /// always inserted as auto-accepted regardless).
    pub member_ids: Vec<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct AcceptDmBody {
    pub dm_channel_id: String,
    /// The accepting member; when signed it must equal the authenticated user
    /// (you accept your OWN pending request, never someone else's).
    pub user_id: String,
    pub accepted_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct AddDmMemberBody {
    pub dm_channel_id: String,
    /// The user being added.
    pub user_id: String,
    /// The actor performing the add; when signed it must equal the authenticated
    /// user, and that user must already be a member of the DM.
    pub added_by: String,
    pub added_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveDmMemberBody {
    pub dm_channel_id: String,
    /// The user being removed.
    pub user_id: String,
    /// The actor performing the removal; when signed it must equal the
    /// authenticated user. Authz: the actor may remove only themselves OR (as the
    /// channel creator) another member.
    pub requester_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct LeaveDmBody {
    pub dm_channel_id: String,
    /// The leaving member; when signed it must equal the authenticated user
    /// (you may only remove your OWN membership).
    pub user_id: String,
}

/// `POST /v1/blocks/add` — add a user to the caller's own block list.
///
/// A transparent newtype over [`BlockBody`] so each endpoint has its own path type.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AddBlock(pub BlockBody);

/// `POST /v1/blocks/remove` — remove a user from the caller's own block list.
/// See [`AddBlock`].
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoveBlock(pub BlockBody);

/// The caller's read cursors, encrypted, for the other devices on the account
/// (#844).
///
/// Unlike [`SavePreferencesBody`], an opaque AEAD blob: read cursors reveal reading habits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveReadCursorsBody {
    /// The owner; when signed it must equal the authenticated user (you sync
    /// only your OWN cursors).
    pub user_id: String,
    /// base64 — the 12-byte AES-256-GCM nonce.
    pub nonce: String,
    /// base64 — AES-256-GCM ciphertext + tag over the padded cursor list.
    pub blob: String,
}

/// `POST /v1/channel-mutes/set` — mute or unmute one group channel for the
/// caller's whole account.
///
/// Stored in the clear because the DS's push fan-out acts on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetChannelMuteBody {
    /// The owner; when signed it must equal the authenticated user (you mute
    /// only for yourself).
    pub user_id: String,
    /// A group CHANNEL id the caller is a member of. A DM or a group id is
    /// refused: a DM is personal by construction and always notifies.
    pub channel_id: String,
    /// `true` mutes, `false` unmutes. Both are idempotent.
    pub muted: bool,
}

/// `POST /v1/read/channel-mutes` — the caller's muted channel ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMutesBody {
    /// Must equal the authenticated user; a mismatch is a 403.
    pub user_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMutesResponse {
    /// Every channel the caller has muted and is still a member of, sorted.
    pub channel_ids: Vec<String>,
}
