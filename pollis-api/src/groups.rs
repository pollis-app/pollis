//! Groups, channels, membership, invites, invite links and join requests.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct CreateGroupBody {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The creator; bound to the authenticated user when signed.
    #[serde(default)]
    pub owner_id: Option<String>,
    /// When present, also create a default `#General` text channel with this id.
    #[serde(default)]
    pub default_text_channel_id: Option<String>,
    /// When present, also create a default `Voice Chat` voice channel with this id.
    #[serde(default)]
    pub default_voice_channel_id: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct UpdateGroupBody {
    pub group_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteGroupBody {
    pub group_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LeaveGroupBody {
    pub group_id: String,
    /// The leaver; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateChannelBody {
    pub id: String,
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub channel_type: String,
    /// The creator; bound to the authenticated user when signed.
    #[serde(default)]
    pub creator_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct UpdateChannelBody {
    pub channel_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteChannelBody {
    pub channel_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveMemberBody {
    pub group_id: String,
    /// The member being removed.
    pub user_id: String,
    /// The actor; bound to the authenticated user when signed.
    #[serde(default)]
    pub requester_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SetMemberRoleBody {
    pub group_id: String,
    /// The member whose role is being changed.
    pub user_id: String,
    pub role: String,
    /// The actor; bound to the authenticated user when signed.
    #[serde(default)]
    pub requester_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateInviteBody {
    pub id: String,
    pub group_id: String,
    /// The inviter; bound to the authenticated user when signed.
    #[serde(default)]
    pub inviter_id: Option<String>,
    /// Who to invite, as the USER TYPED IT — a username or an email (#987).
    ///
    /// Was a resolved `invitee_id`, which made the client run five reads before
    /// it could even build this body: resolve the identifier, check the block
    /// pair, check existing membership, check for a pending invite, and its own
    /// admin preflight. Every one of those is re-derived by this write anyway.
    /// Taking the identifier moves the resolution to where the decision already
    /// lives, and turns six round trips into one.
    pub invitee_identifier: String,
}

/// `POST /v1/invites/create` — the outcome of inviting someone (#987).
///
/// Named outcomes rather than one 403, because the five reads this replaces
/// existed to tell them apart for the user. Every message the client used to
/// produce locally has a variant here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InviteCreated {
    Ok {
        /// The resolved invitee, for the caller's follow-up inbox nudge.
        invitee_id: String,
        /// The inviter's username and the group's name, for the invitee's
        /// status-bar alert (#396) — public directory metadata the DS has
        /// already read to authorize the invite.
        #[serde(default)]
        inviter_username: Option<String>,
        #[serde(default)]
        group_name: Option<String>,
    },
    /// No user matches the identifier.
    NoSuchUser,
    /// The inviter named themselves.
    SelfInvite,
    /// A block exists in either direction. One generic outcome for both
    /// directions, so neither side can infer who blocked whom.
    Blocked,
    /// The invitee is already in the group.
    AlreadyMember,
    /// A pending invite for this pair already exists.
    AlreadyInvited,
}

#[derive(Serialize, Deserialize)]
pub struct AcceptInviteBody {
    pub invite_id: String,
    /// The invitee; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DeclineInviteBody {
    pub invite_id: String,
    /// The invitee; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateJoinRequestBody {
    pub id: String,
    pub group_id: String,
    /// The requester; bound to the authenticated user when signed.
    #[serde(default)]
    pub requester_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ApproveJoinRequestBody {
    pub request_id: String,
    /// The approver; bound to the authenticated user when signed.
    #[serde(default)]
    pub approver_id: Option<String>,
    pub reviewed_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct RejectJoinRequestBody {
    pub request_id: String,
    /// The approver; bound to the authenticated user when signed.
    #[serde(default)]
    pub approver_id: Option<String>,
    pub reviewed_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct CreateInviteLinkBody {
    pub id: String,
    pub group_id: String,
    /// The creator; bound to the authenticated user when signed.
    #[serde(default)]
    pub created_by: Option<String>,
    /// Public lookup handle. Generated client-side.
    pub selector: String,
    /// `sha256(secret)`, hex. The DS never receives the secret at create time —
    /// the client mints the token and hashes it locally.
    pub secret_hash: String,
    /// RFC3339. `None` = never expires.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// `None` = unlimited.
    #[serde(default)]
    pub max_uses: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct RevokeInviteLinkBody {
    pub link_id: String,
    /// The revoker; bound to the authenticated user when signed.
    #[serde(default)]
    pub actor_id: Option<String>,
    pub revoked_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct RedeemInviteLinkBody {
    /// The full `<selector>.<secret>` token as presented by the user.
    pub token: String,
    /// The redeemer; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Client-generated id for the audit row.
    pub attempt_id: String,
    /// RFC3339 "now", used for the expiry comparison and the audit stamp.
    pub now: String,
}

/// `POST /v1/invite-links/redeem` — the group the token admitted the caller to.
///
/// Every way a token can fail to be live answers with ONE indistinguishable
/// rejection status, deliberately (#847), so this type describes the success
/// case only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RedeemInviteLinkResponse {
    Ok {
        group_id: String,
        /// The group's display name, for the caller's confirmation UI (#987).
        ///
        /// Returned with the join rather than fetched after it: the DS has just
        /// read the row to authorize the redemption, and the client asking again
        /// was a second round trip for a string the server already had in hand.
        /// `None` only if the row vanished between the join and this read.
        #[serde(default)]
        group_name: Option<String>,
    },
}

/// `POST /v1/groups/update` — the row as it now stands (#987).
///
/// Returned rather than re-read, for the reason the whole read migration
/// exists: the client used to `SELECT` the group straight back after the write,
/// which was a second round trip for a row the server had just written and,
/// worse, a read that could observe a LATER concurrent update and report it as
/// the result of THIS one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum UpdatedGroup {
    Ok {
        id: String,
        name: String,
        #[serde(default)]
        description: Option<String>,
        owner_id: String,
        created_at: String,
    },
}

/// `POST /v1/channels/update` — the row as it now stands (#987). Same reasoning
/// as [`UpdatedGroup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum UpdatedChannel {
    Ok {
        id: String,
        group_id: String,
        name: String,
        #[serde(default)]
        description: Option<String>,
        channel_type: String,
    },
}

/// `POST /v1/invites/accept` — which group the invite admitted the caller to
/// (#987).
///
/// The client used to read `group_invite` to learn this BEFORE posting the
/// accept, which is both a round trip and a TOCTOU: the invite it read could be
/// revoked before the write landed, leaving the client announcing membership of
/// a group it never joined. Naming the group in the RESPONSE makes the answer a
/// property of the write that succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AcceptedInvite {
    Ok { group_id: String },
}

/// `POST /v1/join-requests/{approve,reject}` — who the reviewed request was
/// for (#987).
///
/// The client used to read the request row before posting, both to learn the
/// group (for the reconcile and the realtime nudge that follow) and to run its
/// admin preflight against it. The write already resolves the same row under the
/// same admin gate, so returning what it resolved deletes the read AND the race
/// where the row changed in between.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ReviewedJoinRequest {
    Ok {
        group_id: String,
        /// The user the request was from. Absent on the reject path, which has
        /// no follow-up that needs it.
        #[serde(default)]
        requester_id: Option<String>,
    },
}

/// `POST /v1/join-requests/create` — the outcome of asking to join (#987).
///
/// The client used to run three reads before posting: does the group exist, am
/// I already a member, and do I already have a PENDING request. All three are
/// re-derived by the write anyway, so what the reads bought was a specific
/// error message. Naming the outcomes here keeps every message and adds none of
/// the reads back — and closes the TOCTOU the third one had, where a request
/// created between the check and the write was silently reset to pending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JoinRequestCreated {
    /// Created, or an earlier rejected/approved row reset to pending.
    Ok,
    /// No such group.
    NoSuchGroup,
    /// The requester is already a member — there is nothing to request.
    AlreadyMember,
    /// A pending request already exists. Distinct from `Ok` because re-asking
    /// is not an error the user should see as success, and distinct from
    /// `AlreadyMember` because it is a different thing to tell them.
    AlreadyPending,
}
