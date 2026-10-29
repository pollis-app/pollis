//! The directory and conversation READ surface (#987).
//!
//! Each endpoint answers a whole dependent chain in one request (opening a
//! channel was 5–7 sequential round trips). Every response is scoped to the
//! caller by the same predicates the DS enforces on writes (`writes::is_member`,
//! `groups::is_admin`), and narrow projections are deliberate: [`UserWire`] has
//! no `phone`, [`GroupPreview`] no `owner_id`. Membership comes from
//! `group_member` / `dm_channel_member`, never the stale `user_groups` /
//! `user_dms` (#540).

use serde::{Deserialize, Serialize};

/// Resolve a conversation to its MLS group, enumerate everything that group
/// backs, and optionally return the envelopes this device has not fetched
/// (opt-in: several callers need only the resolution).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchUpBody {
    /// A group, channel or DM channel id (one namespace, #880); the server
    /// decides which.
    pub conversation_id: String,
    /// Fetch envelopes past each bound conversation's watermark for this device.
    #[serde(default)]
    pub want_envelopes: bool,
    /// Include the DM row and its members (ingest re-pins peers' keys, the DM
    /// screen renders them). Ignored for groups.
    #[serde(default)]
    pub want_dm: bool,
    /// Include the desired MLS roster: `group_member` plus pending invitees (so
    /// they get a Welcome at invite time), or `dm_channel_member`.
    ///
    /// The one server-side definition both `reconcile` and `migrate` use; if
    /// they disagreed, a migration would strand members.
    #[serde(default)]
    pub want_roster: bool,
    /// No-auth (dev/test) path only; ignored when the DS enforces auth.
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
    /// A channel inside a group; all of a group's channels share one MLS group,
    /// keyed by the group id.
    Channel,
    /// A group id given directly.
    Group,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchUpResponse {
    /// `false` → not a member, and every other field is empty. Not a 403: callers
    /// treat "not a member" as an ordinary short-circuit.
    pub authorized: bool,
    /// The MLS group backing the conversation: the group id for a channel, the
    /// DM id for a DM.
    pub mls_group_id: String,
    pub kind: ConversationKind,
    /// Every conversation the MLS group backs, whose envelopes must be decrypted
    /// at each epoch before the shared group advances.
    pub conversation_ids: Vec<String>,
    /// Ordered `(conversation_id, sent_at, id)`, each past that conversation's
    /// own watermark for this device.
    #[serde(default)]
    pub envelopes: Vec<EnvelopeWire>,
    /// The DM and its members. `None` unless `want_dm` and `kind == Dm`.
    #[serde(default)]
    pub dm: Option<DmChannelWire>,
    /// The desired MLS roster. Empty unless `want_roster`.
    #[serde(default)]
    pub roster: Vec<String>,
    /// The commit log's head, read before `envelopes` (#1041); `None` unless
    /// `want_envelopes` (or from an older DS).
    ///
    /// The receiver's half of the epoch gate: envelopes are stored only at the
    /// head epoch, so every envelope below this head is already in `envelopes`,
    /// and a replay of commits `(generation, epoch) < head` never advances past
    /// unfetched envelopes.
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

/// One `message_envelope` row as it crosses the wire; `ciphertext` stays opaque.
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
    /// Display metadata only (#1087): not the delivery order, the cursor, or what
    /// retention compares; `seq` is all three.
    pub sent_at: String,
    /// The envelope's `type` column (`message`, `edit`, `delete`, …).
    pub kind: String,
    /// The DS-assigned delivery position, strictly increasing per conversation
    /// (#1087), and the cursor (`seq > last_seq`). `Option` for the wire only; a
    /// row without one is skipped rather than delivered out of order.
    #[serde(default)]
    pub seq: Option<i64>,
    /// Acceptance order across all conversations of one MLS group (the rowid).
    /// Decryption needs it: a sender's generations run across every channel, and
    /// openmls drops a generation more than 5 behind the newest it opened.
    /// Absent from an older DS, which falls back to per-conversation order.
    #[serde(default)]
    pub arrival: Option<i64>,
}

/// Which conversation an envelope belongs to, for deleting a message this device
/// never fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLookupBody {
    pub message_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Also return reactions on these messages in `conversation_id`: the same
    /// authorization question, answered once per page instead of per row.
    #[serde(default)]
    pub reactions_for: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLookupResponse {
    /// `None` when the envelope does not exist or the caller is not a member;
    /// not a 403, which would make this an existence oracle for message ids.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// One entry per message in `reactions_for` that has reactions and that the
    /// caller may see.
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
    /// The caller's stored preferences JSON, if any.
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
    /// A group id or a channel id, resolved server-side like `writes::is_member`,
    /// so a moderation preflight needs one request.
    pub group_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Answer only `exists` / `authorized` / `role` and skip every list.
    #[serde(default)]
    pub role_only: bool,
    /// Include the group's custom emoji (members only).
    #[serde(default)]
    pub want_emoji: bool,
    /// Include pending invites (#1506; members only, opt-in). An older DS omits
    /// them, which reads as "none pending".
    #[serde(default)]
    pub want_invites: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryGroupResponse {
    /// The group id the request resolved to, echoed even in `role_only` mode
    /// and when `exists` is false, so channel-id callers need not resolve again.
    pub group_id: String,
    /// The id resolved to a real group. Separate from `authorized`: "no such
    /// group" and "already a member" are different refusals.
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
    /// Pending invites when `want_invites` asked (#1506). Every member sees
    /// them: invitees are already in the desired MLS roster.
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

/// An invite link, minus its token: only `sha256(secret)` is stored and the
/// selector is not returned, so this cannot recover an uncopied link.
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
    /// Uses the redeem path's `datetime()` normalisation, so the badge matches
    /// what redemption will do.
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

/// The Cmd+K fan-out: member lists for several groups at once, each authorized
/// independently.
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
    /// `false` → not a member of this group; `members` is empty.
    pub authorized: bool,
    pub members: Vec<GroupMemberWire>,
}

/// Profile hydration and username/email lookup (same projection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryUsersBody {
    /// Hydrate these ids.
    #[serde(default)]
    pub user_ids: Vec<String>,
    /// Resolve one username-or-email to a user; exact match, not search.
    #[serde(default)]
    pub identifier: Option<String>,
    /// Which of these users are in a block relationship with the caller, in
    /// either direction. Direction-blind on purpose, so neither party can infer
    /// who blocked whom.
    #[serde(default)]
    pub block_check: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryUsersResponse {
    /// Everyone found from ids and identifier, deduplicated; unresolved ids are
    /// absent.
    pub users: Vec<UserWire>,
    /// The subset of `block_check` that is blocked in either direction.
    #[serde(default)]
    pub blocked: Vec<String>,
}

/// A user's public directory projection. No `phone`: nothing renders it and it
/// is the most re-identifying field on the row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserWire {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub preferred_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// Slug discovery, the join flow's first step. Deliberately not
/// membership-gated (#917), as with Discord vanity invites; the projection and
/// a rate limit protect it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupBySlugBody {
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupBySlugResponse {
    #[serde(default)]
    pub group: Option<GroupPreview>,
}

/// Exactly what a join prompt needs to name a group: no `owner_id`, no
/// `created_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupPreview {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A group's URL slug, derived from its display name.
///
/// Shared so the DS's slug lookup and the client agree (#987). Lowercase, keep
/// only ASCII alphanumerics / whitespace / `-`, whitespace runs become `-`,
/// collapse and trim `-`. Non-ASCII letters are dropped (`café` -> `caf`), as
/// every shipped link was minted.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConversationsBody {
    #[serde(default)]
    pub user_id: Option<String>,
    /// Also resolve the caller's own 1:1 DM with this user (`call-*` rooms use
    /// the DM's MLS group); it cannot ask about two other people.
    #[serde(default)]
    pub dm_with_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConversationsResponse {
    pub group_ids: Vec<String>,
    pub dm_ids: Vec<String>,
    /// The 1:1 DM with `dm_with_user_id`, if asked for and one exists.
    #[serde(default)]
    pub dm_with: Option<String>,
}
