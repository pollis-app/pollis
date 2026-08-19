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
    pub invitee_id: String,
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
