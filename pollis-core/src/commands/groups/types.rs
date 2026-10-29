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
///
/// The slug lookup cannot be membership-gated (you search for a group you are
/// not in), so the guard is this shape: a distinct struct, so a field added to
/// [`Group`] cannot leak here. Carries only what the join prompt renders.
/// `owner_id` (names a user), `created_at` and any member count are
/// deliberately absent.
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
    // 'text' or 'voice'.
    pub channel_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupWithChannels {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub created_at: String,
    pub current_user_role: String,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GroupMember {
    pub user_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub joined_at: String,
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
    pub inviter_id: String,
    pub inviter_username: Option<String>,
    pub created_at: String,
}

/// A freshly minted invite link (#847).
///
/// `token` and `url` exist only in this value, returned once; the server holds
/// only `sha256(secret)`, so nobody can re-read a link. To share again, mint a
/// new one.
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
///
/// Carries no token field, so re-reading a link's secret is unrepresentable.
#[derive(Debug, Serialize, Deserialize)]
pub struct InviteLinkSummary {
    pub id: String,
    pub created_at: String,
    pub creator_username: Option<String>,
    pub expires_at: Option<String>,
    pub max_uses: Option<i64>,
    pub uses: i64,
    pub revoked_at: Option<String>,
    /// Server-evaluated, against the same clock redemption is judged by.
    pub is_live: bool,
}

/// The outcome of redeeming an invite link.
#[derive(Debug, Serialize, Deserialize)]
pub struct RedeemedInvite {
    pub group_id: String,
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
