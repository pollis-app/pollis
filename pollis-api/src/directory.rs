//! The directory and conversation READ surface (#987).
//!
//! Sidebar, conversation and envelope reads, asked of the DS (no client database token).
//!
//! # Why these shapes
//!
//! **They collapse dependent chains:** [`CatchUpBody`] answers four dependent questions in one request.
//!
//! **They are authorization-shaped:** scoped by the same predicates as the writes; [`UserWire`] has no `phone`, [`GroupPreview`] no `owner_id`.
//!
//! **They do not read `user_groups` / `user_dms`** (stale since #540); membership comes from `group_member` / `dm_channel_member`.

use serde::{Deserialize, Serialize};

/// Resolve a conversation to its MLS group, enumerate everything that group
/// backs, and (optionally) hand back the envelopes this device has not fetched.
///
/// `want_envelopes` is opt-in because the envelope scan is the expensive half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchUpBody {
    /// A group, channel or DM id (one namespace, #880); the server decides which.
    pub conversation_id: String,
    /// Fetch envelopes past each bound conversation's own watermark for this
    /// device.
    #[serde(default)]
    pub want_envelopes: bool,
    /// Include the DM row and its members. Ignored for groups.
    ///
    /// One flag for TOFU re-pinning and the DM screen's roster.
    #[serde(default)]
    pub want_dm: bool,
    /// Include the conversation's DESIRED MLS ROSTER: the user ids that should
    /// have a leaf in its tree.
    ///
    /// Members plus pending invitees (so they get a Welcome at invite time); the ONE definition `reconcile` and `migrate` share.
    #[serde(default)]
    pub want_roster: bool,
    /// No-auth (dev/test) path only; ignored when the DS enforces auth, though
    /// still signature-bound.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// What kind of thing the requested id turned out to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationKind {
    /// A DM channel. Its MLS group is keyed by the DM id itself.
    Dm,
    /// A channel inside a group. Every channel of a group shares ONE MLS group,
    /// keyed by the group id.
    Channel,
    /// A group id given directly (the sweep and the MLS paths address groups
    /// this way).
    Group,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchUpResponse {
    /// `false` → not a member; every other field is empty (an ordinary short-circuit, not a 403).
    pub authorized: bool,
    /// The MLS group backing the conversation: the group id for a channel, the
    /// DM id for a DM.
    pub mls_group_id: String,
    pub kind: ConversationKind,
    /// Every conversation the MLS group backs; all must be decrypted per epoch before it advances.
    pub conversation_ids: Vec<String>,
    /// Ordered `(conversation_id, sent_at, id)`, each past that conversation's own watermark for this device.
    #[serde(default)]
    pub envelopes: Vec<EnvelopeWire>,
    /// The DM and its members. `None` unless `want_dm` and `kind == Dm`.
    #[serde(default)]
    pub dm: Option<DmChannelWire>,
    /// The desired MLS roster. Empty unless `want_roster`.
    #[serde(default)]
    pub roster: Vec<String>,
    /// The commit log's head, read BEFORE `envelopes` was (#1041). `None` unless
    /// `want_envelopes` (or from a DS that predates the field).
    ///
    /// The receiver's half of the epoch gate: replaying only commits before this head never skips unfetched envelopes.
    #[serde(default)]
    pub head: Option<LogHead>,
}

/// A position in the commit log: the next epoch a commit in `generation` would
/// be accepted from (`MAX(epoch) + 1`, or `0` for an empty lineage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogHead {
    pub generation: i64,
    pub epoch: i64,
}

/// One `message_envelope` row as it crosses the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeWire {
    pub conversation_id: String,
    pub id: String,
    pub sender_id: String,
    pub ciphertext: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    #[serde(default)]
    pub target_message_id: Option<String>,
    /// DISPLAY metadata only since #1087; `seq` is the order, cursor and retention key.
    pub sent_at: String,
    /// The envelope's `type` column — `message`, `edit`, `delete`, …. Named
    /// `kind` here because `type` is a Rust keyword.
    pub kind: String,
    /// The DS-assigned delivery position (#1087), the cursor: next fetch is `seq > last_seq`.
    #[serde(default)]
    pub seq: Option<i64>,
    /// The DS's acceptance order across the group's conversations; decrypting per conversation loses messages.
    #[serde(default)]
    pub arrival: Option<i64>,
}

/// Which conversation an envelope belongs to.
///
/// Used by the delete path when this device holds no local copy of the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLookupBody {
    pub message_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Also return the reactions on these messages, in `conversation_id`.
    ///
    /// Batched here because it is the same authorization question.
    #[serde(default)]
    pub reactions_for: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLookupResponse {
    /// `None` when the envelope does not exist OR the caller is not a member of
    /// its conversation.
    ///
    /// Not a 403 for non-members: that would be an existence oracle for message ids.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// One entry per message in `reactions_for` that HAS reactions and is visible to the caller.
    #[serde(default)]
    pub reactions: Vec<MessageReactions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReactions {
    pub message_id: String,
    pub reactions: Vec<ReactionWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionWire {
    pub emoji: String,
    pub user_ids: Vec<String>,
}

/// Everything the sidebar needs at launch, actor-scoped, in one request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryBootstrapBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryBootstrapResponse {
    pub groups: Vec<GroupWithChannelsWire>,
    /// Accepted DMs, minus any whose other members the caller has blocked.
    pub dms: Vec<DmChannelWire>,
    /// Un-accepted DMs, same block filter.
    pub dm_requests: Vec<DmChannelWire>,
    pub pending_invites: Vec<PendingInviteWire>,
    pub blocks: Vec<BlockedUserWire>,
    /// The caller's stored preferences JSON, or `None` when they have never
    /// saved any.
    #[serde(default)]
    pub preferences: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupWire {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub owner_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelWire {
    pub id: String,
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub channel_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupWithChannelsWire {
    #[serde(flatten)]
    pub group: GroupWire,
    pub channels: Vec<ChannelWire>,
    /// The CALLER's role in this group (`admin` / `member`).
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmChannelWire {
    pub id: String,
    pub created_by: String,
    pub created_at: String,
    pub members: Vec<DmMemberWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmMemberWire {
    pub user_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub added_by: Option<String>,
    #[serde(default)]
    pub added_at: Option<String>,
    #[serde(default)]
    pub accepted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInviteWire {
    pub id: String,
    pub group_id: String,
    pub group_name: String,
    pub inviter_id: String,
    #[serde(default)]
    pub inviter_username: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedUserWire {
    pub user_id: String,
    #[serde(default)]
    pub username: Option<String>,
    pub blocked_at: String,
}

/// One group's detail page, gated per section: members and channels need
/// membership, invite links and join requests need admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryGroupBody {
    /// A group or CHANNEL id, resolved server-side so a moderation preflight is one request.
    pub group_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Answer only `exists` / `authorized` / `role` and skip every list.
    #[serde(default)]
    pub role_only: bool,
    /// Include the group's custom emoji set (members-only).
    #[serde(default)]
    pub want_emoji: bool,
    /// Include pending invites (#1506); opt-in, and an older DS answers none.
    #[serde(default)]
    pub want_invites: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryGroupResponse {
    /// The GROUP id the request resolved to.
    ///
    /// Echoed always, so a caller that passed a CHANNEL id learns the group without resolving twice.
    pub group_id: String,
    /// The id resolved to a real GROUP.
    ///
    /// Separate from `authorized`: "no such group" and "already a member" are different refusals.
    pub exists: bool,
    /// `false` → not a member; everything below is empty.
    pub authorized: bool,
    /// The caller's role, `None` when not a member.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub group: Option<GroupWire>,
    pub channels: Vec<ChannelWire>,
    pub members: Vec<GroupMemberWire>,
    /// Admin-only; empty for a plain member.
    pub invite_links: Vec<InviteLinkWire>,
    /// Admin-only; empty for a plain member.
    pub join_requests: Vec<JoinRequestWire>,
    #[serde(default)]
    pub emoji: Vec<CustomEmojiWire>,
    /// Pending invites when asked (#1506); visible to every member, as invitees are in the roster.
    #[serde(default)]
    pub invites: Vec<GroupInviteWire>,
}

/// A pending invite as the group's member page lists it (#1506): who was
/// invited, by whom, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInviteWire {
    pub id: String,
    pub invitee_id: String,
    #[serde(default)]
    pub invitee_username: Option<String>,
    pub inviter_id: String,
    #[serde(default)]
    pub inviter_username: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberWire {
    pub user_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    pub role: String,
    pub joined_at: String,
}

/// An invite link, minus its token.
///
/// Neither the secret (never stored) nor the selector is returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteLinkWire {
    pub id: String,
    pub created_at: String,
    #[serde(default)]
    pub creator_username: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub max_uses: Option<i64>,
    pub uses: i64,
    #[serde(default)]
    pub revoked_at: Option<String>,
    /// Same `datetime()` normalisation as redeem, so the badge matches redemption.
    pub is_live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequestWire {
    pub id: String,
    pub group_id: String,
    pub requester_id: String,
    #[serde(default)]
    pub requester_username: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEmojiWire {
    pub group_id: String,
    pub group_name: String,
    pub shortcode: String,
    pub content_hash: String,
    pub content_type: String,
    pub animated: bool,
    pub size_bytes: i64,
    pub created_by: String,
}

/// The Cmd+K fan-out: member lists for several groups at once.
///
/// Each group is authorized independently, so one group the caller left does not fail the batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryMembersBody {
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryMembersResponse {
    pub groups: Vec<GroupMembersWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMembersWire {
    pub group_id: String,
    /// `false` → not a member of THIS group; `members` is empty.
    pub authorized: bool,
    pub members: Vec<GroupMemberWire>,
}

/// Profile hydration and username/email lookup.
///
/// One endpoint for both; the caller supplies ids, an identifier, or both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryUsersBody {
    /// Hydrate these ids.
    #[serde(default)]
    pub user_ids: Vec<String>,
    /// Resolve one username-or-email to a user (exact match).
    #[serde(default)]
    pub identifier: Option<String>,
    /// Which of these users are in a block relationship with the CALLER, in
    /// EITHER direction.
    ///
    /// Direction-blind, so neither party can infer who blocked whom.
    #[serde(default)]
    pub block_check: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryUsersResponse {
    /// Everyone found, from ids and identifier together, deduplicated. A
    /// requested id that does not resolve is simply absent.
    pub users: Vec<UserWire>,
    /// The subset of `block_check` that is blocked in either direction.
    #[serde(default)]
    pub blocked: Vec<String>,
}

/// A user's public directory projection.
///
/// **No `phone`:** nothing renders it, and it is the most re-identifying field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserWire {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub preferred_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// Slug discovery — the join flow's first step.
///
/// Deliberately NOT membership-gated (#917); the protection is the narrow projection plus a rate limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupBySlugBody {
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupBySlugResponse {
    #[serde(default)]
    pub group: Option<GroupPreview>,
}

/// Exactly what a join prompt needs to name a group, and nothing else — no
/// `owner_id` (a named user), no `created_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupPreview {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}


/// A group's URL slug, derived from its display name.
///
/// Shared (#987) so client and DS derive identical slugs; it cannot be expressed in SQL.
///
/// Lowercase, keep ASCII alnum/whitespace/`-`, whitespace → `-`, collapse and trim `-`; non-ASCII is DROPPED.
pub fn derive_slug(name: &str) -> String {
    let lower = name.to_lowercase();
    let cleaned: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace() || *c == '-')
        .collect();
    let with_hyphens = cleaned.split_ascii_whitespace().collect::<Vec<_>>().join("-");
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in with_hyphens.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result.trim_matches('-').to_string()
}

/// Id-only enumeration of everything the caller participates in.
///
/// For the cold-launch sweep, account deletion and enrollment finalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConversationsBody {
    #[serde(default)]
    pub user_id: Option<String>,
    /// Also resolve the 1:1 DM channel between the caller and this user.
    ///
    /// For `call-*` rooms, whose MLS group is the DM's; scoped to the caller's own DMs.
    #[serde(default)]
    pub dm_with_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConversationsResponse {
    pub group_ids: Vec<String>,
    pub dm_ids: Vec<String>,
    /// The 1:1 DM channel with `dm_with_user_id`, if one exists. `None` when
    /// the caller did not ask, or when the two share no two-person DM.
    #[serde(default)]
    pub dm_with: Option<String>,
}
