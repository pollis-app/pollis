//! Group / channel / membership commands, re-exported at
//! `pollis_core::commands::groups::*`.

pub mod authz;
mod channels;
// Renaming `groups::groups` would churn every `pub use` below.
#[allow(clippy::module_inception)]
mod groups;
mod invite_token;
mod invites;
mod join_requests;
mod membership;
mod types;

// Slugs are `pollis_api::directory::derive_slug`, matched on the DS (#987).

pub use types::{
    Channel, CreatedInviteLink, Group, GroupInvite, GroupMember, GroupPreview,
    GroupWithChannels, InviteLinkSummary, JoinRequest, PendingInvite, RedeemedInvite,
};

pub use groups::{
    create_group, delete_group, list_user_groups, list_user_groups_with_channels,
    search_group_by_slug, update_group,
};

pub use channels::{
    create_channel, delete_channel, list_group_channels, list_muted_channels, set_channel_muted,
    update_channel,
};

pub use membership::{
    get_group_members, leave_group, remove_member_from_group, set_member_role,
};

pub use invites::{
    accept_group_invite, create_group_invite_link, decline_group_invite, get_pending_invites,
    list_group_invite_links, list_group_invites, redeem_group_invite_link, revoke_group_invite,
    revoke_group_invite_link, send_group_invite, INVITE_LINK_ERR,
};

pub use join_requests::{
    approve_join_request, get_group_join_requests, get_my_join_request, reject_join_request,
    request_group_access,
};

#[cfg(test)]
mod tests;
