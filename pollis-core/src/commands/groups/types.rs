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
/// `search_group_by_slug` is the discovery step of the join flow, so it cannot
/// take a membership check without making itself useless — you search for a
/// group precisely because you are not in it yet. The guard therefore has to be
/// the SHAPE rather than the caller, and this type is that guard: a distinct
/// struct, not a trimmed [`Group`], so "which fields may an outsider see" is
/// answered once by the type instead of being re-decided at each call site.
/// A field added to `Group` cannot leak here by accident.
///
/// Present, because the join prompt renders them: `name` and `description`
/// (what the user is asking to join) and `id` (the argument to
/// `request_group_access`; already known to anyone who can reach the row, and
/// useless without an approving admin).
///
/// Deliberately absent:
///   * `owner_id` — names a specific user. It is the roster leak of #917 in
///     miniature: one member, but a member, and reliably the admin.
///   * `created_at` — no UI consumes it; a stranger has no business profiling
///     when a group was formed.
///   * Member count — deliberately NOT added. It has no consumer in the join
///     prompt, and a per-group population figure is exactly the aggregate a
///     slug-enumerating client would want. This narrows the disclosure; it must
///     not widen it under cover of the same change.
///
/// The slug is not returned: it is `derive_slug(name)`, and both the renderer
/// (`urlRouting.ts`) and this crate already derive it from the name.
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
    // Migration: ALTER TABLE channels ADD COLUMN channel_type TEXT NOT NULL DEFAULT 'text';
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

/// A freshly minted invite link (#847).
///
/// `token` and `url` exist ONLY in this value, returned once from
/// `create_group_invite_link`. They are never persisted and never recoverable —
/// the server holds only `sha256(secret)`. That is the direct, intended cost of
/// "a database read must not yield working invites": nobody, including the
/// creator, can re-read a link after the fact. To share again, mint a new one.
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
/// Deliberately carries NO token field — the type makes "re-read an existing
/// link's secret" unrepresentable rather than merely unimplemented.
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

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinRequest {
    pub id: String,
    pub group_id: String,
    pub requester_id: String,
    pub requester_username: Option<String>,
    pub status: String,
    pub created_at: String,
}
