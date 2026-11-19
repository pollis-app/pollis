//! Group / channel / membership commands — split into cohesive submodules.

mod aliases;
pub mod authz;
mod channels;
// `groups::groups` holds the group CRUD proper, the others its neighbours;
// renaming it would churn every `pub use` below for no reader's benefit.
#[allow(clippy::module_inception)]
mod groups;
mod invite_token;
mod invites;
mod join_requests;
mod membership;
mod types;

pub use types::{
    Channel, ChannelCategory, CreatedInviteLink, Group, GroupInvite, GroupMember, GroupPreview,
    GroupWithChannels, InviteLinkSummary, InvitePreview, JoinRequest, PendingInvite,
    RedeemedInvite,
};

pub use groups::{
    create_group, delete_group, list_user_groups, list_user_groups_with_channels,
    search_group_by_slug, update_group,
};

pub use aliases::{
    list_group_aliases, list_my_aliases, reset_group_alias, set_group_alias, GroupAlias,
};
pub(crate) use aliases::{cache_group_aliases, cached_alias_name};

pub use channels::{
    create_channel, create_channel_category, delete_channel, delete_channel_category,
    list_group_channels, list_muted_channels, move_channel,
    move_channel_category, rename_channel_category, set_channel_muted, update_channel,
};

pub use membership::{
    get_group_members, leave_group, remove_member_from_group, set_member_role,
};

pub use invites::{
    accept_group_invite, create_group_invite_link, decline_group_invite, get_pending_invites, invite_to_group,
    list_group_invite_links, list_group_invites, preview_group_invite_link,
    redeem_group_invite_link, revoke_group_invite,
    revoke_group_invite_link, send_group_invite, INVITE_LINK_ERR,
};

pub use join_requests::{
    approve_join_request, get_group_join_requests, get_my_join_request, reject_join_request,
    request_group_access,
};

#[cfg(test)]
mod tests;
