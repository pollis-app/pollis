//! Cold-launch (or post-reconnect) MLS catch-up sweep.
//!
//! Single backend command that, given a user_id, enumerates every MLS
//! group the user is in (regular groups + DMs) and runs the catch-up
//! sequence (`poll_mls_welcomes_inner` once + `catch_up_mls_group_interleaved`
//! per group) so the local MLS state matches the server's published epoch —
//! decrypting every message sealed en route, not just replaying commits — before
//! the user can take any MLS-powered action.
//!
//! Closes the cold-launch race documented in issue #371 scenario 5: between
//! sign-in / unlock and the first time any per-call catch-up fires, a user
//! action (send_message, edit_message, voice join, screen-share) could run
//! against a stale epoch. Calling this at AppShell mount and awaiting it
//! before unlocking interactive UI closes that window.
//!
//! Best-effort per group: a single group's failure (e.g. revoked device,
//! transient Turso error) logs and continues to the next so one bad row
//! never blocks the rest of the sweep.
//!
//! ## Eviction/remove reconcile backstop (issue #430 P1)
//!
//! The MLS post that evicts a removed member from the ratchet tree
//! (`remove_member_from_group` / `remove_user_from_dm_channel` → `reconcile`)
//! is best-effort: if it is dropped, the removed user is deleted from the
//! server roster but LINGERS in every remaining member's LOCAL tree — still a
//! recipient of the seals for new messages — until some *unrelated* membership
//! change happens to run reconcile again. That is a forward-secrecy gap, the
//! eviction-side analog of the bootstrap gap fixed in #427.
//!
//! So after catching a group up, the sweep also runs a reconcile backstop:
//! it retries the dropped remove/eviction so the removed device is actually
//! pruned from the local tree. It is gated behind a cheap local-vs-roster
//! pre-check ([`local_tree_has_stale_leaf`]) so in steady state — when the tree
//! already matches the roster — it costs only two lightweight SELECTs and a
//! local MLS load, never the heavyweight reconcile (KP claims, account-key
//! pinning, commit crypto).

use std::collections::HashSet;
use std::sync::Arc;

use openmls::prelude::*;

use crate::error::Result;
use crate::state::AppState;

use super::provider::{parse_credential_device_id, parse_credential_user_id, PollisProvider};

pub async fn catch_up_all_mls_groups(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let device_id = state.device_id.lock().await.clone();
    if let Some(ref did) = device_id {
        if let Err(e) =
            crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
        {
            eprintln!("[mls-sweep] poll_mls_welcomes: {e}");
        }
    }

    let conn = state.remote_db.conn().await?;

    let mut group_ids: Vec<String> = Vec::new();
    let mut rows = conn
        .query(
            "SELECT g.id FROM groups g \
             JOIN group_member gm ON gm.group_id = g.id \
             WHERE gm.user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        group_ids.push(row.get::<String>(0)?);
    }
    drop(rows);

    let mut dm_ids: Vec<String> = Vec::new();
    let mut rows = conn
        .query(
            "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        dm_ids.push(row.get::<String>(0)?);
    }
    drop(rows);

    eprintln!(
        "[mls-sweep] {user_id}: {} group(s), {} dm(s)",
        group_ids.len(),
        dm_ids.len()
    );

    // Regular groups: mls_group_id IS the group id. Route through the group-level
    // interleaved catch-up (not a bare commit-only replay) so a returning offline
    // member decrypts every message sealed at an epoch it's about to advance past,
    // rather than losing anything sent before a membership change during its
    // offline window. Interleaved ingest still advances the group to head, so the
    // cold-launch guarantee (#371) is preserved.
    for gid in &group_ids {
        if let Err(e) =
            crate::commands::messages::catch_up_mls_group_interleaved(state, gid, user_id).await
        {
            eprintln!("[mls-sweep] catch_up_mls_group for group {gid}: {e}");
        }
        // Backstop a dropped remove/eviction commit (issue #430 P1).
        if let Err(e) = reconcile_backstop(state, gid, user_id).await {
            eprintln!("[mls-sweep] reconcile backstop for group {gid}: {e}");
        }
    }

    // DMs: mls_group_id IS the dm_channel_id — a single-conversation MLS group.
    for did in &dm_ids {
        if let Err(e) =
            crate::commands::messages::catch_up_mls_group_interleaved(state, did, user_id).await
        {
            eprintln!("[mls-sweep] catch_up_mls_group for dm {did}: {e}");
        }
        // Backstop a dropped remove/eviction commit (issue #430 P1).
        if let Err(e) = reconcile_backstop(state, did, user_id).await {
            eprintln!("[mls-sweep] reconcile backstop for dm {did}: {e}");
        }
    }

    Ok(())
}

/// Eviction/remove reconcile backstop for a single conversation (issue #430 P1).
///
/// Retries a dropped remove/eviction MLS commit so a member deleted from the
/// server roster is actually pruned from THIS device's local ratchet tree —
/// closing the forward-secrecy gap where the removed device would otherwise keep
/// decrypting new messages until an unrelated membership change ran reconcile.
///
/// Cheap in steady state: the heavyweight [`reconcile_group_mls_impl`] only runs
/// when the local tree still holds a leaf the roster no longer justifies
/// ([`local_tree_has_stale_leaf`]). When the tree already matches the roster this
/// is a near-no-op (two SELECTs + a local MLS load, no DS round-trips).
///
/// Ordering / locking: the caller runs the interleaved ingesting catch-up for
/// this conversation immediately before this call, so the current epoch's
/// messages are ingested BEFORE reconcile can advance the epoch (mirrors how
/// `membership.rs` / `invites.rs` hoist catch-up above reconcile). We hold no MLS
/// lock across the call — `reconcile_group_mls_impl` takes the per-conversation
/// `mls_group_lock` for its whole body, and its own lost-race converge re-runs
/// the interleaved catch-up — so there is no deadlock and no epoch advanced past
/// an un-ingested message.
async fn reconcile_backstop(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> Result<()> {
    if !local_tree_has_stale_leaf(state, conversation_id).await? {
        return Ok(());
    }

    eprintln!(
        "[mls-sweep] {conversation_id}: stale leaf in local tree — retrying dropped remove/eviction reconcile"
    );
    crate::commands::mls::reconcile_group_mls_impl(state, conversation_id, user_id).await?;
    Ok(())
}

/// Cheap pre-check for the reconcile backstop: does the LOCAL MLS ratchet tree
/// still contain a leaf that a declarative reconcile would evict — a device whose
/// user has left the roster, or whose `user_device` row is gone?
///
/// Returns `false` (the steady-state answer) using only a local MLS load and two
/// lightweight roster/device SELECTs — never the DS claim / account-key-pin /
/// commit-crypto work of a full reconcile. The "stale" test mirrors
/// `reconcile_group_mls_impl`'s roster + `valid_devices` derivation exactly, so
/// it flags precisely the leaves reconcile would drop and never misses a pending
/// eviction.
async fn local_tree_has_stale_leaf(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<bool> {
    // 1. Local tree membership — a local SQLite read, no network. Scope the
    //    !Send provider/group so neither crosses an await.
    let tree_members: Vec<(String, String)> = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            // No local group open (never joined / already forgotten): nothing to evict.
            None => return Ok(false),
        };
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        match MlsGroup::load(provider.storage(), &group_id) {
            Ok(Some(group)) => group
                .members()
                .map(|m| {
                    let uid = parse_credential_user_id(&m.credential);
                    let did = parse_credential_device_id(&m.credential).unwrap_or_default();
                    (uid, did)
                })
                .collect(),
            // Missing / unreadable local group: nothing this device can evict.
            _ => return Ok(false),
        }
    };
    if tree_members.is_empty() {
        return Ok(false);
    }

    let conn = state.remote_db.conn().await?;

    // 2. Desired roster: group_member + pending invitees, or dm_channel_member.
    //    Mirrors `reconcile_group_mls_impl` — pending invitees count as desired
    //    so their as-yet-unjoined leaves are never mistaken for stale ones.
    let mut roster: HashSet<String> = HashSet::new();
    {
        let mut rows = conn
            .query(
                "SELECT user_id FROM group_member WHERE group_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster.insert(row.get::<String>(0)?);
        }
    }
    {
        let mut rows = conn
            .query(
                "SELECT invitee_id FROM group_invite WHERE group_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster.insert(row.get::<String>(0)?);
        }
    }
    if roster.is_empty() {
        let mut rows = conn
            .query(
                "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster.insert(row.get::<String>(0)?);
        }
    }

    // 3. Valid (user_id, device_id) pairs still registered for the roster — the
    //    same `user_device` snapshot reconcile uses to drop revoked single
    //    devices of a still-present user.
    let mut valid_devices: HashSet<(String, String)> = HashSet::new();
    {
        let safe_ids: Vec<String> = roster
            .iter()
            .map(|id| {
                id.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                    .collect::<String>()
            })
            .collect();
        if !safe_ids.is_empty() {
            let in_clause = safe_ids
                .iter()
                .map(|id| format!("'{id}'"))
                .collect::<Vec<_>>()
                .join(",");
            let query =
                format!("SELECT user_id, device_id FROM user_device WHERE user_id IN ({in_clause})");
            let mut rows = conn.query(&query, ()).await?;
            while let Some(row) = rows.next().await? {
                valid_devices.insert((row.get::<String>(0)?, row.get::<String>(1)?));
            }
        }
    }

    // 4. A leaf is stale iff its user left the roster OR its device row is gone —
    //    exactly the leaves reconcile would remove. Any such leaf means a
    //    remove/eviction commit was dropped and must be retried.
    Ok(tree_members
        .iter()
        .any(|(uid, did)| !roster.contains(uid) || !valid_devices.contains(&(uid.clone(), did.clone()))))
}
