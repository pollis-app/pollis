use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub created_at: String,
}

/// What a stranger may learn about a group by naming its slug (#917).
/// A distinct type so a field added to `Group` cannot leak to outsiders.
#[derive(Debug, Serialize, Deserialize)]
pub struct GroupPreview {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub description: Option<String>,
    // 'text' or 'voice' — persisted in Turso.
    pub channel_type: String,
    /// The category it sits in (#2031); `None` = the uncategorised block.
    #[serde(default)]
    pub category_id: Option<String>,
}

/// A group's channel category (#2031). Group-wide and admin-managed; lists
/// carry them in display order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCategory {
    pub id: String,
    pub group_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupWithChannels {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub created_at: String,
    pub current_user_role: String,
    /// In display order (#2031): uncategorised first, then category by category.
    pub channels: Vec<Channel>,
    /// In display order (#2031).
    #[serde(default)]
    pub categories: Vec<ChannelCategory>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GroupMember {
    pub user_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub joined_at: String,
    /// #1182: a bot; the UI shows the Bot badge.
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingInvite {
    pub id: String,
    pub group_id: String,
    pub group_name: String,
    pub inviter_id: String,
    pub inviter_username: Option<String>,
    pub created_at: String,
}

/// A pending invite as the group's member page lists it (#1506).
#[derive(Debug, Serialize, Deserialize)]
pub struct GroupInvite {
    pub id: String,
    pub invitee_id: String,
    pub invitee_username: Option<String>,
    /// #2085: the invitee's display name, with the row.
    #[serde(default)]
    pub invitee_preferred_name: Option<String>,
    pub inviter_id: String,
    pub inviter_username: Option<String>,
    pub created_at: String,
}

/// A freshly minted invite link (#847).
/// `token` and `url` are returned once, never persisted; the server keeps only `sha256(secret)`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreatedInviteLink {
    pub id: String,
    /// The full `<selector>.<secret>` token. Show once.
    pub token: String,
    /// The shareable URL wrapping `token`.
    pub url: String,
    pub expires_at: Option<String>,
    pub max_uses: Option<i64>,
}

/// An invite link as shown in the group's management list.
/// Deliberately carries no token: an existing link's secret is unrecoverable.
#[derive(Debug, Serialize, Deserialize)]
pub struct InviteLinkSummary {
    pub id: String,
    pub created_at: String,
    pub creator_username: Option<String>,
    pub expires_at: Option<String>,
    pub max_uses: Option<i64>,
    pub uses: i64,
    pub revoked_at: Option<String>,
    /// Server-evaluated liveness, so the UI never re-derives expiry from a
    /// clock that may disagree with the one redemption is judged against.
    pub is_live: bool,
}

/// The outcome of redeeming an invite link.
#[derive(Debug, Serialize, Deserialize)]
pub struct RedeemedInvite {
    pub group_id: String,
    pub group_name: Option<String>,
}

/// What an invite link would admit the caller to, before they choose to join (#1972).
#[derive(Debug, Serialize, Deserialize)]
pub struct InvitePreview {
    /// `None` when the group has no row any more, or the DS predates the preview.
    pub group_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinRequest {
    pub id: String,
    pub group_id: String,
    pub requester_id: String,
    pub requester_username: Option<String>,
    pub status: String,
    pub created_at: String,
}
