//! Group / channel / membership commands — split into cohesive submodules.
//! Public surface is preserved via the `pub use` re-exports below so every
//! external caller (Tauri shims, sibling `commands::*` modules, integration
//! tests) keeps resolving names at `pollis_core::commands::groups::*`.

mod channels;
mod groups;
mod invites;
mod join_requests;
mod membership;
mod types;

/// Mirrors the frontend `deriveSlug` in urlRouting.ts.
pub(super) fn derive_slug(name: &str) -> String {
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

// ── Types ────────────────────────────────────────────────────────────────────
pub use types::{Channel, Group, GroupMember, GroupWithChannels, JoinRequest, PendingInvite};

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
    accept_group_invite, decline_group_invite, get_pending_invites, send_group_invite,
};

// ── Join requests ────────────────────────────────────────────────────────────
pub use join_requests::{
    approve_join_request, get_group_join_requests, get_my_join_request, reject_join_request,
    request_group_access,
};

#[cfg(test)]
mod tests;
