//! Profile, preferences, blocks and DM conversations.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

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
/// A `#[serde(transparent)]` newtype over [`BlockBody`] for the same reason as
/// [`crate::messages::AddReaction`]: add and remove share one field list, but the
/// path is a property of the type, so each endpoint needs its own.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AddBlock(pub BlockBody);

/// `POST /v1/blocks/remove` — remove a user from the caller's own block list.
/// See [`AddBlock`].
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoveBlock(pub BlockBody);
