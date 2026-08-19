//! Group / channel / membership commands — split into cohesive submodules.
//! Public surface is preserved via the `pub use` re-exports below so every
//! external caller (Tauri shims, sibling `commands::*` modules, integration
//! tests) keeps resolving names at `pollis_core::commands::groups::*`.

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

// A group's URL slug is `pollis_api::directory::derive_slug`, and since #987
// nothing in THIS crate derives it: slug MATCHING moved to the DS
// (`POST /v1/directory/group-by-slug`), which is what finally makes the rate
// limit on it meaningful. The rule lives in the shared crate because two copies
// of a name-normalisation rule that must agree exactly is exactly the drift that
// makes a shared link resolve on one build and 404 on the next. The renderer's
// `deriveSlug` in `urlRouting.ts` is the third copy, and it only ever BUILDS
// urls — it never has to agree with a stored value.

// ── Types ────────────────────────────────────────────────────────────────────
pub use types::{
    Channel, CreatedInviteLink, Group, GroupMember, GroupPreview, GroupWithChannels,
    InviteLinkSummary, JoinRequest, PendingInvite, RedeemedInvite,
};

// ── Group CRUD / search ──────────────────────────────────────────────────────
pub use groups::{
    create_group, delete_group, list_user_groups, list_user_groups_with_channels,
    search_group_by_slug, update_group,
};

// ── Channel CRUD ─────────────────────────────────────────────────────────────
pub use channels::{create_channel, delete_channel, list_group_channels, update_channel};

// ── Membership / roles ───────────────────────────────────────────────────────
pub use membership::{
    get_group_members, leave_group, remove_member_from_group, set_member_role,
};

// ── Invites ──────────────────────────────────────────────────────────────────
pub use invites::{
    accept_group_invite, create_group_invite_link, decline_group_invite, get_pending_invites,
    list_group_invite_links, redeem_group_invite_link, revoke_group_invite_link,
    send_group_invite, INVITE_LINK_ERR,
};

// ── Join requests ────────────────────────────────────────────────────────────
pub use join_requests::{
    approve_join_request, get_group_join_requests, get_my_join_request, reject_join_request,
    request_group_access,
};

#[cfg(test)]
mod tests;
