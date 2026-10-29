//! Profile, preferences, blocks and DM conversations.
//!
//! Wire types only; see the matching `pollis-delivery` module for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct UpdateProfileBody {
    /// The profile being edited; when signed it must equal the authenticated user.
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
    /// The owner; when signed it must equal the authenticated user.
    pub user_id: String,
    pub preferences: String,
}

#[derive(Serialize, Deserialize)]
pub struct BlockBody {
    /// The blocker; when signed it must equal the authenticated user.
    pub blocker_id: String,
    pub blocked_id: String,
}

/// `POST /v1/dm/create`: the DM, new or already existing (#987). The 2-person
/// dedupe happens inside the write, so concurrent creates cannot make two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CreatedDm {
    Ok {
        id: String,
        created_by: String,
        created_at: String,
        members: Vec<DmMember>,
        /// `true` when an existing 2-person DM was returned; the caller then
        /// skips MLS group creation.
        existing: bool,
    },
    /// A proposed pairing is blocked in either direction; one generic refusal,
    /// so neither side can infer why.
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
    /// Every participant; the creator is always inserted as auto-accepted.
    pub member_ids: Vec<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct AcceptDmBody {
    pub dm_channel_id: String,
    /// The accepting member; when signed it must equal the authenticated user.
    pub user_id: String,
    pub accepted_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct AddDmMemberBody {
    pub dm_channel_id: String,
    /// The user being added.
    pub user_id: String,
    /// The actor; when signed it must equal the authenticated user, who must
    /// already be a member.
    pub added_by: String,
    pub added_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveDmMemberBody {
    pub dm_channel_id: String,
    /// The user being removed.
    pub user_id: String,
    /// The actor; when signed it must equal the authenticated user. They may
    /// remove only themselves or, as channel creator, another member.
    pub requester_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct LeaveDmBody {
    pub dm_channel_id: String,
    /// The leaving member; when signed it must equal the authenticated user.
    pub user_id: String,
}

/// `POST /v1/blocks/add`. A transparent newtype over [`BlockBody`], as
/// [`crate::messages::AddReaction`].
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AddBlock(pub BlockBody);

/// `POST /v1/blocks/remove`; see [`AddBlock`].
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoveBlock(pub BlockBody);

/// The caller's read cursors for the account's other devices (#844).
///
/// Unlike [`SavePreferencesBody`] (plaintext, fine for a theme name), this is an
/// opaque AEAD blob: cursors record which conversations a person reads and when.
/// The server has no key (`pollis_core::commands::messages::read_state`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveReadCursorsBody {
    /// The owner; when signed it must equal the authenticated user.
    pub user_id: String,
    /// base64 12-byte AES-256-GCM nonce.
    pub nonce: String,
    /// base64 AES-256-GCM ciphertext + tag over the padded cursor list.
    pub blob: String,
}

/// `POST /v1/channel-mutes/set`: mute or unmute one group channel for the whole
/// account (#1431). Stored in the clear because the DS acts on it: the push
/// fan-out skips muted recipients unless directly `@`-mentioned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetChannelMuteBody {
    /// The owner; when signed it must equal the authenticated user.
    pub user_id: String,
    /// A group channel id the caller belongs to. DMs and group ids are refused:
    /// a DM always notifies.
    pub channel_id: String,
    /// `true` mutes, `false` unmutes. Both are idempotent.
    pub muted: bool,
}

/// `POST /v1/read/channel-mutes`: the caller's muted channel ids.
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
