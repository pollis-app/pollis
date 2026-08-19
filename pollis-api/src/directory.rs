//! The directory and conversation READ surface (#987).
//!
//! Everything the app needs to draw a sidebar, open a conversation, or pull the
//! envelopes it has not seen — asked of the Delivery Service instead of of a
//! whole-database read token baked into the shipped binary.
//!
//! # Why these shapes
//!
//! **They collapse dependent chains, not just queries.** The reads these replace
//! were sequential *and* dependent: each query's input was the previous one's
//! output, so the client could not pipeline them. Opening a channel was 5–7
//! round trips, a cold launch with eight groups and twelve DMs was sixty to
//! eighty, serially. Co-locating the data would have shortened each hop; running
//! the chain next to the database removes the hops. So [`CatchUpBody`] answers
//! "which MLS group backs this conversation", "is it a DM", "which conversations
//! does that group cover" and "what have I not fetched" in ONE request, because
//! those four questions were four dependent trips.
//!
//! **They are authorization-shaped, not table-shaped.** Every response here is
//! scoped to the authenticated caller by a predicate the DS already enforces on
//! the WRITE half of the same table (`writes::is_member`, `groups::is_admin`), so
//! no new authorization primitive exists to get wrong. Where a projection is
//! narrower than the row, that is deliberate and load-bearing: [`UserWire`]
//! carries no `phone`, and [`GroupPreview`] carries no `owner_id`.
//!
//! **They do not read `user_groups` / `user_dms`.** Those tables were the #532
//! directory index; #540 reverted it and removed every writer, so they now hold a
//! one-time backfill minus deletions. An endpoint reading them would serve a
//! confidently wrong sidebar. Membership is derived from `group_member` /
//! `dm_channel_member`, which is what the rest of the system agrees with.

use serde::{Deserialize, Serialize};

// ── POST /v1/conversations/catch-up ──────────────────────────────────────────

/// Resolve a conversation to its MLS group, enumerate everything that group
/// backs, and (optionally) hand back the envelopes this device has not fetched.
///
/// One request in place of the four dependent ones it replaces. `want_envelopes`
/// is opt-in because several callers — send, edit, the voice join, the commit
/// replay entry point — need only the resolution, and the envelope scan is the
/// expensive half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchUpBody {
    /// A group id, a channel id, or a DM channel id. All three share one
    /// namespace (`conversation` registry, #880), so the server decides which it
    /// is rather than the caller asserting it.
    pub conversation_id: String,
    /// Fetch envelopes past each bound conversation's own watermark for this
    /// device.
    #[serde(default)]
    pub want_envelopes: bool,
    /// Include the other members of a DM, so the caller can re-pin their
    /// identity keys (TOFU). Ignored for groups.
    #[serde(default)]
    pub want_dm_peers: bool,
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
    /// `false` → the caller is not a member. Every other field is empty. Not a
    /// 403, because several callers treat "not a member" as an ordinary
    /// short-circuit rather than an error, exactly as the direct read's
    /// membership-joined query did by returning no row.
    pub authorized: bool,
    /// The MLS group backing the conversation: the group id for a channel, the
    /// DM id for a DM.
    pub mls_group_id: String,
    pub kind: ConversationKind,
    /// Every conversation the MLS group backs — the DM itself, or all of the
    /// group's channels. This is the set whose envelopes must be decrypted at
    /// each epoch before the shared group advances past it.
    pub conversation_ids: Vec<String>,
    /// Ordered `(conversation_id, sent_at, id)`, each strictly past THAT
    /// conversation's own watermark for this device — byte-for-byte the
    /// ordering the interleaved ingest partitions on.
    #[serde(default)]
    pub envelopes: Vec<EnvelopeWire>,
    /// The DM's other members (never the caller). Empty unless
    /// `want_dm_peers` and `kind == Dm`.
    #[serde(default)]
    pub dm_peers: Vec<String>,
}

/// One `message_envelope` row as it crosses the wire.
///
/// `ciphertext` is already an opaque string on the wire and stays one: the DS
/// cannot read it, and nothing here changes that.
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
    pub sent_at: String,
    /// The envelope's `type` column — `message`, `edit`, `delete`, …. Named
    /// `kind` here because `type` is a Rust keyword.
    pub kind: String,
}

// ── POST /v1/messages/lookup ─────────────────────────────────────────────────

/// Which conversation an envelope belongs to.
///
/// Used by the delete path when this device holds no local copy of the target —
/// an admin deleting a message they never fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLookupBody {
    pub message_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLookupResponse {
    /// `None` when the envelope does not exist OR the caller is not a member of
    /// its conversation.
    ///
    /// Deliberately not a 403 for the second case: a distinguishable refusal
    /// would make this an existence oracle for message ids, and the caller's
    /// behaviour is identical either way — it cannot name the conversation, so
    /// it cannot delete.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

// ── POST /v1/directory/bootstrap ─────────────────────────────────────────────

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

// ── POST /v1/directory/group ─────────────────────────────────────────────────

/// One group's detail page, gated per section: members and channels need
/// membership, invite links and join requests need admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryGroupBody {
    pub group_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Include the group's custom emoji set. Members-only, and a separate flag
    /// because the picker asks for emoji across ALL the caller's groups through
    /// a different endpoint.
    #[serde(default)]
    pub want_emoji: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryGroupResponse {
    /// `false` → not a member. Everything below is empty, which is the same
    /// answer a stranger got from the membership-gated read this replaces.
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
/// The secret half never existed server-side (only `sha256(secret)` is stored),
/// and the selector is not returned either — this is the management list, not a
/// way to recover a link that was not copied when it was made.
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
    /// Computed with the same `datetime()` normalisation the redeem path uses,
    /// so the badge cannot disagree with what redemption will actually do.
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

// ── POST /v1/directory/members ───────────────────────────────────────────────

/// The Cmd+K fan-out: member lists for several groups at once.
///
/// The palette used to run a membership gate query AND a member query per group
/// — `1 + 2N` round trips to open a menu. Each group is authorized
/// independently, so one group the caller has left does not fail the batch.
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

// ── POST /v1/directory/users ─────────────────────────────────────────────────

/// Profile hydration and username/email lookup.
///
/// One endpoint for both because they answer the same projection; the caller
/// supplies ids, an identifier, or both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryUsersBody {
    /// Hydrate these ids.
    #[serde(default)]
    pub user_ids: Vec<String>,
    /// Resolve one username-or-email to a user. Matching is exact, as it was
    /// against the database — this is a lookup, not a search index.
    #[serde(default)]
    pub identifier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryUsersResponse {
    /// Everyone found, from ids and identifier together, deduplicated. A
    /// requested id that does not resolve is simply absent.
    pub users: Vec<UserWire>,
}

/// A user's public directory projection.
///
/// **No `phone`.** The column exists and the old `SELECT` returned it to every
/// caller, including the ones that only wanted an avatar. Nothing in the app
/// renders it, and a phone number is the single most re-identifying field on the
/// row — so it does not cross this boundary at all, rather than crossing it and
/// being ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserWire {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub preferred_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

// ── POST /v1/directory/group-by-slug ─────────────────────────────────────────

/// Slug discovery — the join flow's first step.
///
/// Deliberately NOT membership-gated (#917): a `require_member` guard would mean
/// you could only find groups you were already in, which is not a stricter
/// version of the feature but the removal of it. Discord's vanity-invite lookup
/// and Slack's workspace-URL lookup make the same trade. The protection is the
/// PROJECTION plus a rate limit: the existence of a group at a guessed name is
/// public, its owner and contents are not.
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
/// # Why this lives in the shared crate (#987)
///
/// Slug matching moved server-side when `search_group_by_slug` became
/// `POST /v1/directory/group-by-slug`: the DS now scans group names and compares
/// derived slugs, where the client used to. The rule is not expressible in SQL
/// (it strips, folds and collapses), so it is Rust on whichever side runs it —
/// and if the two sides ever computed it differently, a link that worked in one
/// build would silently resolve to nothing in the next. One implementation, both
/// ends, exactly as `pollis_schema::authz`'s statements do for the role checks.
///
/// The rule, unchanged from the client implementation it replaces: lowercase,
/// drop everything that is not ASCII alphanumeric / whitespace / `-`, turn runs
/// of whitespace into single `-`, collapse repeated `-`, trim leading and
/// trailing `-`. Non-ASCII letters are DROPPED rather than transliterated
/// (`café` -> `caf`), which is the behaviour every shipped link was minted
/// against.
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

// ── POST /v1/directory/conversations ─────────────────────────────────────────

/// Id-only enumeration of everything the caller participates in.
///
/// The cold-launch sweep, account deletion's membership-change broadcast, and
/// device-enrollment finalization all want the same list and none of them wants
/// a row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConversationsBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConversationsResponse {
    pub group_ids: Vec<String>,
    pub dm_ids: Vec<String>,
}
