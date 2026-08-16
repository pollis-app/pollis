use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::derive_slug;
use super::types::{Channel, Group, GroupPreview, GroupWithChannels};
use super::authz;

pub async fn list_user_groups_with_channels(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupWithChannels>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT g.id, g.name, g.description, g.owner_id, g.created_at,
                c.id, c.group_id, c.name, c.description, c.channel_type,
                gm.role
         FROM groups g
         JOIN group_member gm ON gm.group_id = g.id
         LEFT JOIN channels c ON c.group_id = g.id
         WHERE gm.user_id = ?1
         ORDER BY g.created_at, c.name",
        libsql::params![user_id],
    ).await?;

    let mut groups: Vec<GroupWithChannels> = Vec::new();
    while let Some(row) = rows.next().await? {
        let group_id: String = row.get(0)?;
        let channel_id: Option<String> = row.get(5)?;

        if let Some(existing) = groups.iter_mut().find(|g| g.id == group_id) {
            if let Some(cid) = channel_id {
                existing.channels.push(Channel {
                    id: cid,
                    group_id: row.get(6)?,
                    name: row.get(7)?,
                    description: row.get(8)?,
                    channel_type: row.get::<Option<String>>(9)?.unwrap_or_else(|| "text".to_string()),
                });
            }
        } else {
            let mut channels = Vec::new();
            if let Some(cid) = channel_id {
                channels.push(Channel {
                    id: cid,
                    group_id: row.get(6)?,
                    name: row.get(7)?,
                    description: row.get(8)?,
                    channel_type: row.get::<Option<String>>(9)?.unwrap_or_else(|| "text".to_string()),
                });
            }
            groups.push(GroupWithChannels {
                id: group_id,
                name: row.get(1)?,
                description: row.get(2)?,
                owner_id: row.get(3)?,
                created_at: row.get(4)?,
                current_user_role: row.get::<Option<String>>(10)?.unwrap_or_else(|| "member".to_string()),
                channels,
            });
        }
    }

    Ok(groups)
}

pub async fn list_user_groups(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Group>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT g.id, g.name, g.description, g.owner_id, g.created_at
         FROM groups g
         JOIN group_member gm ON gm.group_id = g.id
         WHERE gm.user_id = ?1",
        libsql::params![user_id],
    ).await?;

    let mut groups = Vec::new();
    while let Some(row) = rows.next().await? {
        groups.push(Group {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            owner_id: row.get(3)?,
            created_at: row.get(4)?,
        });
    }

    Ok(groups)
}

pub async fn create_group(
    name: String,
    description: Option<String>,
    owner_id: String,
    // Opt-in to auto-creating a #General text channel. Defaults to false
    // — the user is expected to set these toggles in the create-group
    // form. Tauri elides the param entirely when omitted by the caller.
    create_default_text_channel: Option<bool>,
    // Opt-in to auto-creating a Voice Chat voice channel.
    create_default_voice_channel: Option<bool>,
    state: &Arc<AppState>,
) -> Result<Group> {
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // Default channel ids are generated up front so the same value goes into the
    // DB write (direct or DS) regardless of which path runs.
    let text_channel_id =
        create_default_text_channel.unwrap_or(false).then(|| Ulid::new().to_string());
    let voice_channel_id =
        create_default_voice_channel.unwrap_or(false).then(|| Ulid::new().to_string());

    // Route the group + admin-member + default-channel inserts through the
    // Delivery Service (one transactional, server-authorized write).
    let body = pollis_api::groups::CreateGroupBody {
        id: id.clone(),
        name: name.clone(),
        description: description.clone(),
        owner_id: Some(owner_id.clone()),
        default_text_channel_id: text_channel_id,
        default_voice_channel_id: voice_channel_id,
        created_at: now.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Create the per-group MLS group — all channels in this group share it.
    match crate::commands::mls::init_mls_group(state, &id, &owner_id).await {
        Ok(()) => {
            // Reconcile adds the creator's other devices (if any have KPs).
            if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
                state, &id, &owner_id,
            ).await {
                eprintln!("[mls] create_group: reconcile failed: {e}");
            }
        }
        Err(e) => eprintln!("[mls] create_group: mls group init failed (non-fatal): {e}"),
    }

    Ok(Group { id, name, description, owner_id, created_at: now })
}

pub async fn update_group(
    group_id: String,
    requester_id: String,
    name: Option<String>,
    description: Option<String>,
    icon_url: Option<String>,
    state: &Arc<AppState>,
) -> Result<Group> {
    let conn = state.remote_db.conn().await?;

    authz::require_admin(&conn, &group_id, &requester_id, "update group settings").await?;

    // Route the column updates through the Delivery Service (which re-derives the
    // admin role server-side).
    let body = pollis_api::groups::UpdateGroupBody {
        group_id: group_id.clone(),
        requester_id: Some(requester_id),
        name,
        description,
        icon_url,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Announce the rename/icon change to the group (#874). Without this the
    // only thing that refreshed another member's sidebar was a window-focus
    // refetch — a poll wearing an event's clothes.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] update_group: notify group {group_id}: {e}");
    }

    let mut rows = conn.query(
        "SELECT id, name, description, owner_id, created_at FROM groups WHERE id = ?1",
        libsql::params![group_id],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Group {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            owner_id: row.get(3)?,
            created_at: row.get(4)?,
        })
    } else {
        Err(Error::Other(anyhow::anyhow!("group not found after update")))
    }
}

pub async fn delete_group(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    authz::require_admin(&conn, &group_id, &requester_id, "delete the group").await?;

    // CASCADE deletes group_member and channels entries. Route the delete through
    // the Delivery Service (admin re-checked server-side).
    let body = pollis_api::groups::DeleteGroupBody {
        group_id,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Find a group whose name derives to the given slug.
/// Returns an error if no match is found.
///
/// #917: deliberately NOT membership-gated, and deliberately narrowed instead.
///
/// This is the discovery step of the join flow — `SearchGroupPage` calls it,
/// then offers `request_group_access` on what it finds. A `require_member`
/// guard here would mean you could only find groups you were already in, which
/// is not a stricter version of the feature, it is the removal of the feature.
/// Discord's vanity-invite lookup and Slack's workspace-URL lookup make the
/// same trade: the existence of a group at a guessed name is public, its
/// contents are not.
///
/// So the fix is the projection. This used to hand back the whole [`Group`] row
/// — including `owner_id`, a named user — to anyone who guessed a slug. It now
/// returns [`GroupPreview`], which carries exactly what the join prompt
/// renders. See that type for the field-by-field reasoning.
///
/// What this endpoint still discloses, unavoidably, is whether a group with a
/// given slug exists; that is inherent to slug lookup and is the same bet the
/// products above take.
///
/// The scan below is O(rows) per query — `derive_slug` is Rust, not SQL, so it
/// cannot be an indexed lookup. That is a cost worth knowing about, but it is
/// deliberately NOT described here as "wants a rate limit": this query runs on
/// the client, against Turso, under the caller's own read token, so a limit
/// applied at this function is one the caller can decline to apply. Rate
/// limiting slug lookup would mean moving the lookup behind the DS. See the
/// bound documented in `authz.rs` — it applies to this projection exactly as it
/// applies to the `require_member` guards.
pub async fn search_group_by_slug(
    slug: String,
    state: &Arc<AppState>,
) -> Result<GroupPreview> {
    let conn = state.remote_db.conn().await?;
    let target = slug.trim().to_lowercase();

    // Only the three columns `GroupPreview` carries are SELECTed. Narrowing the
    // query as well as the struct means a future edit to the mapping cannot
    // reach a sensitive column without also editing the SQL.
    let mut rows = conn.query(
        "SELECT id, name, description FROM groups",
        libsql::params![],
    ).await?;

    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if derive_slug(&name) == target {
            return Ok(GroupPreview {
                id: row.get(0)?,
                name,
                description: row.get(2)?,
            });
        }
    }

    Err(Error::Other(anyhow::anyhow!("No group found with slug '{}'", slug)))
}
