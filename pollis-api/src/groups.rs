//! Groups, channels, membership, invites, invite links and join requests.
//!
//! Wire types only; see `pollis-delivery` for handling.

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
    pub invitee_identifier: String,
}

/// `POST /v1/invites/create` — the outcome of inviting someone (#987).
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

/// Withdraw a pending invite (#1506). Allowed to the member who sent it and to
/// any admin of the invite's group, both re-derived server-side from the invite
/// row — never from the request.
#[derive(Serialize, Deserialize)]
pub struct RevokeInviteBody {
    pub invite_id: String,
    /// The revoker; bound to the authenticated user when signed.
    #[serde(default)]
    pub actor_id: Option<String>,
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
    /// **IGNORED by the server, and kept only for wire compatibility.**
    ///
    /// Expiry and rate limits use the server clock; shipped clients still send it.
    pub now: String,
}

/// `POST /v1/invite-links/redeem` — the group the token admitted the caller to.
///
/// Every failure is one indistinguishable status (#847), so this is the success case only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RedeemInviteLinkResponse {
    Ok {
        group_id: String,
        /// The group's display name, for the caller's confirmation UI (#987).
        ///
        /// `None` only if the row vanished between the join and this read.
        #[serde(default)]
        group_name: Option<String>,
    },
}

/// `POST /v1/groups/update` — the row as it now stands (#987).
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AcceptedInvite {
    Ok { group_id: String },
}

/// `POST /v1/join-requests/{approve,reject}` — who the reviewed request was
/// for (#987).
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
