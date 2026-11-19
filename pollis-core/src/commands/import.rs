//! Importing a community's structure from another platform: Discord (#1180)
//! and Slack (#1179), through one pipeline.
//!
//! `pollis-import` reads an export from disk and plans; this module points
//! the plan at a target group and applies it, every write a device-signed DS
//! call through the ordinary group commands. Nothing Discord- or
//! Slack-shaped reaches the DS, and no platform credential is ever held: the
//! export was made by the user (DiscordChatExporter on their machine, or the
//! workspace export Slack's admin pages hand out).
//!
//! - **Dry run first.** [`import_preview`] reads, plans against the target's
//!   current state and returns the plan; it writes nothing.
//! - **Idempotent.** [`import_apply`] re-plans against the target (never
//!   trusting a plan from the renderer) and creates only what is missing:
//!   categories by name, channels by (category, name), emoji by picture (the
//!   same re-encode the upload does, so the same content hash). A second run
//!   creates nothing.
//! - **Structure only.** Message history is measured and reported, never
//!   imported. Membership is planned, never imported: an MLS leaf needs an
//!   enrolled device. The plan becomes an invite link, a sheet, and (where
//!   the source shares emails, as Slack does) an ordinary group invite to
//!   each email that belongs to a Pollis account.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pollis_api::groups::InviteCreated;
use pollis_import::model::Media;
use pollis_import::plan::{ExistingChannel, ImportOptions, ImportPlan};
pub use pollis_import::SourceKind;
use serde::{Deserialize, Serialize};

use crate::commands::groups::{self, CreatedInviteLink, GroupWithChannels};
use crate::error::{Error, Result};
use crate::state::AppState;

/// How long the import's invite link lives; the invite page's default.
const INVITE_LINK_HOURS: i64 = 24 * 7;

/// `target` value that asks for a new group.
pub const TARGET_NEW: &str = "new";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportTarget {
    pub group_id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ImportPreview {
    pub source: SourceKind,
    /// The folder or ZIP read, as given.
    pub path: String,
    pub plan: ImportPlan,
    /// The invite sheet (CSV) for this plan.
    pub invite_sheet: String,
    /// The group the plan was made against; `None` = a new group.
    pub target: Option<ImportTarget>,
    /// Groups this user administers whose name matches the source's: where a
    /// re-run should go.
    pub candidates: Vec<ImportTarget>,
    /// Whether archived channels are in this plan (Slack; opt-in).
    pub include_archived: bool,
}

/// The admin's choices for an apply, beyond the target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportApplyOptions {
    /// Bring archived channels too (Slack).
    #[serde(default)]
    pub include_archived: bool,
    /// Send a group invite to every planned email that belongs to a Pollis
    /// account (Slack). Off unless asked.
    #[serde(default)]
    pub invite_by_email: bool,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub group_id: String,
    pub group_name: String,
    pub created_group: bool,
    pub icon_set: bool,
    pub categories_created: u32,
    pub channels_created: u32,
    /// Channels put back in their planned place (a re-run after a partial one).
    pub channels_moved: u32,
    pub emoji_uploaded: u32,
    /// Already there, so left alone.
    pub categories_existing: u32,
    pub channels_existing: u32,
    pub emoji_existing: u32,
    /// Each write that failed; the rest went ahead. Re-running retries them.
    pub failures: Vec<ImportFailure>,
    /// A new invite link (shown once). `None` when the group already had a
    /// live one, which is not minted again.
    pub invite_link: Option<CreatedInviteLink>,
    pub invite_link_existing: bool,
    /// One entry per planned email, when invites by email were asked for.
    pub email_invites: Vec<EmailInvite>,
}

#[derive(Debug, Serialize)]
pub struct ImportFailure {
    /// `category`, `channel`, `emoji`, `icon`, `order`, `invite_link` or `invite`.
    pub kind: String,
    pub name: String,
    pub error: String,
}

/// What became of one planned email invite.
#[derive(Debug, Serialize)]
pub struct EmailInvite {
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub outcome: EmailInviteOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailInviteOutcome {
    /// A Pollis account has this email: it got an ordinary group invite.
    Invited,
    /// Invited by an earlier run; still pending.
    AlreadyInvited,
    AlreadyMember,
    /// The admin's own email.
    You,
    /// No Pollis account answers to this email (or the DS declined to say
    /// more): they join through the invite link.
    NoAccount,
    /// The invite request itself failed (listed in `failures`); a re-run retries it.
    Failed,
}

/// Read the export at `path` and plan it against `target`: `None` picks the
/// one admin-held group whose name matches (a re-run) or else a new group;
/// [`TARGET_NEW`] forces a new group; anything else is a group id.
pub async fn import_preview(
    source: SourceKind,
    path: String,
    user_id: String,
    target: Option<String>,
    include_archived: Option<bool>,
    state: &Arc<AppState>,
) -> Result<ImportPreview> {
    let include_archived = include_archived.unwrap_or(false);
    let export = read_export(source, &path).await?;
    let fresh = ImportOptions { include_archived, ..ImportOptions::default() };
    let base_plan = pollis_import::plan::plan(&export, &fresh);
    let groups = groups::list_user_groups_with_channels(user_id.clone(), state).await?;
    let candidates: Vec<ImportTarget> = groups
        .iter()
        .filter(|g| g.current_user_role == "admin" && g.name.eq_ignore_ascii_case(&base_plan.group.name))
        .map(|g| ImportTarget { group_id: g.id.clone(), name: g.name.clone() })
        .collect();

    let chosen: Option<&GroupWithChannels> = match target.as_deref() {
        Some(TARGET_NEW) => None,
        Some(id) => Some(admin_group(&groups, id)?),
        None if candidates.len() == 1 => groups.iter().find(|g| g.id == candidates[0].group_id),
        None => None,
    };
    let plan = match chosen {
        Some(g) => plan_against(&export, g, include_archived, &user_id, state).await?,
        None => base_plan,
    };
    Ok(ImportPreview {
        source,
        path,
        invite_sheet: pollis_import::report::invite_sheet(&plan),
        plan,
        target: chosen.map(|g| ImportTarget { group_id: g.id.clone(), name: g.name.clone() }),
        candidates,
        include_archived,
    })
}

/// Apply the import at `path` to `target` (as for the preview, except `None`
/// means a new group: the renderer always passes what it previewed).
pub async fn import_apply(
    source: SourceKind,
    path: String,
    user_id: String,
    target: Option<String>,
    options: Option<ImportApplyOptions>,
    state: &Arc<AppState>,
) -> Result<ImportResult> {
    let options = options.unwrap_or_default();
    let export = read_export(source, &path).await?;
    let mut failures = Vec::new();

    let target_id = target.filter(|t| t != TARGET_NEW);
    let (group_id, group_name, created_group, plan) = match target_id {
        Some(id) => {
            let groups = groups::list_user_groups_with_channels(user_id.clone(), state).await?;
            let g = admin_group(&groups, &id)?;
            let plan = plan_against(&export, g, options.include_archived, &user_id, state).await?;
            (g.id.clone(), g.name.clone(), false, plan)
        }
        None => {
            let fresh = ImportOptions { include_archived: options.include_archived, ..ImportOptions::default() };
            let plan = pollis_import::plan::plan(&export, &fresh);
            let g = groups::create_group(plan.group.name.clone(), None, user_id.clone(), Some(false), Some(false), state)
                .await?;
            (g.id, g.name, true, plan)
        }
    };

    // The icon only on a new group: on a re-run an admin may have changed it since.
    let mut icon_set = false;
    if created_group {
        if let Some(Media::Local(path)) = &plan.group.icon {
            match set_group_icon(&group_id, &user_id, path, state).await {
                Ok(()) => icon_set = true,
                Err(e) => failures.push(failure("icon", &plan.group.name, e)),
            }
        }
    }

    // Categories in order; each new one lands at the bottom, so creating in
    // plan order keeps the plan's order.
    let current = group_state(&user_id, &group_id, state).await?;
    let mut category_ids: BTreeMap<String, String> = BTreeMap::new();
    for k in &current.categories {
        category_ids.entry(k.name.to_lowercase()).or_insert_with(|| k.id.clone());
    }
    let mut categories_created = 0;
    let mut categories_existing = 0;
    for k in &plan.categories {
        if category_ids.contains_key(&k.name.to_lowercase()) {
            categories_existing += 1;
            continue;
        }
        match groups::create_channel_category(group_id.clone(), k.name.clone(), user_id.clone(), state).await {
            Ok(created) => {
                category_ids.insert(k.name.to_lowercase(), created.id);
                categories_created += 1;
            }
            Err(e) => failures.push(failure("category", &k.name, e)),
        }
    }

    // Channels, likewise at the bottom of their block.
    let existing: BTreeSet<(Option<String>, String)> = channel_keys(&current);
    let mut channels_created = 0;
    let mut channels_existing = 0;
    for c in &plan.channels {
        let key = (c.category.as_ref().map(|k| k.to_lowercase()), c.name.to_lowercase());
        if existing.contains(&key) {
            channels_existing += 1;
            continue;
        }
        let category_id = match &c.category {
            Some(k) => match category_ids.get(&k.to_lowercase()) {
                Some(id) => Some(id.clone()),
                // Its category failed above; reported there, and a re-run retries both.
                None => continue,
            },
            None => None,
        };
        match groups::create_channel(
            group_id.clone(),
            c.name.clone(),
            c.description.clone(),
            Some(c.kind.as_str().to_string()),
            user_id.clone(),
            category_id,
            state,
        )
        .await
        {
            Ok(_) => channels_created += 1,
            Err(e) => failures.push(failure("channel", &c.name, e)),
        }
    }

    // A fresh group is already in order. After a partial earlier run, a
    // channel created now sits at the bottom of its block: put the planned
    // channels back in plan order, ahead of anything the admin added.
    let mut channels_moved = 0;
    if !created_group && channels_created > 0 && channels_existing > 0 {
        match reorder(&plan, &user_id, &group_id, state).await {
            Ok(n) => channels_moved = n,
            Err(e) => failures.push(failure("order", &group_name, e)),
        }
    }

    let mut emoji_uploaded = 0;
    let mut emoji_existing = 0;
    for e in &plan.emoji {
        let (Some(code), Some(Media::Local(path))) = (&e.shortcode, &e.image) else {
            continue;
        };
        if e.exists {
            emoji_existing += 1;
            continue;
        }
        match crate::commands::emoji::upload_group_emoji(
            group_id.clone(),
            code.clone(),
            path.to_string_lossy().into_owned(),
            state,
        )
        .await
        {
            Ok(_) => emoji_uploaded += 1,
            Err(err) => failures.push(failure("emoji", code, err)),
        }
    }

    // One live invite link is enough; a re-run does not mint another.
    let live = groups::list_group_invite_links(group_id.clone(), user_id.clone(), state)
        .await
        .map(|ls| ls.iter().any(|l| l.is_live))
        .unwrap_or(false);
    let invite_link = if live {
        None
    } else {
        match groups::create_group_invite_link(group_id.clone(), user_id.clone(), Some(INVITE_LINK_HOURS), None, state)
            .await
        {
            Ok(link) => Some(link),
            Err(e) => {
                failures.push(failure("invite_link", &group_name, e));
                None
            }
        }
    };

    // Invites by email: an ordinary group invite to each planned email. The
    // DS resolves the email; one that matches no account answers "no such
    // user" and that person joins through the link. Idempotent: a re-run
    // finds them already invited or already in.
    let mut email_invites = Vec::new();
    if options.invite_by_email {
        for p in plan.people.iter().filter(|p| !p.is_bot) {
            let Some(email) = &p.email else {
                continue;
            };
            let outcome = match groups::invite_to_group(&group_id, &user_id, email, state).await {
                Ok(InviteCreated::Ok { .. }) => EmailInviteOutcome::Invited,
                Ok(InviteCreated::AlreadyInvited) => EmailInviteOutcome::AlreadyInvited,
                Ok(InviteCreated::AlreadyMember) => EmailInviteOutcome::AlreadyMember,
                Ok(InviteCreated::SelfInvite) => EmailInviteOutcome::You,
                Ok(InviteCreated::NoSuchUser | InviteCreated::Blocked) => EmailInviteOutcome::NoAccount,
                Err(e) => {
                    failures.push(failure("invite", &p.display_name, e));
                    EmailInviteOutcome::Failed
                }
            };
            email_invites.push(EmailInvite {
                username: p.username.clone(),
                display_name: p.display_name.clone(),
                email: email.clone(),
                outcome,
            });
        }
    }

    let invited = email_invites.iter().filter(|i| i.outcome == EmailInviteOutcome::Invited).count();
    crate::diag!(
        "[import] {} into {group_id}: +{categories_created} categories, +{channels_created} channels, \
         +{emoji_uploaded} emoji, +{invited} invites, {} failures",
        source.name(),
        failures.len()
    );
    Ok(ImportResult {
        group_id,
        group_name,
        created_group,
        icon_set,
        categories_created,
        channels_created,
        channels_moved,
        emoji_uploaded,
        categories_existing,
        channels_existing,
        emoji_existing,
        failures,
        invite_link,
        invite_link_existing: live,
        email_invites,
    })
}

async fn read_export(source: SourceKind, path: &str) -> Result<pollis_import::model::Export> {
    let at = PathBuf::from(path);
    let export = tokio::task::spawn_blocking(move || pollis_import::read_export(source, &at))
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("read export: {e}")))?
        .map_err(|e| Error::Other(anyhow::anyhow!("could not read {path}: {e}")))?;
    if export.community.id.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(match source {
            SourceKind::Discord =>
                "no Discord export found in that folder (DiscordChatExporter JSON files or discord-guild.json)",
            SourceKind::Slack => "no Slack workspace export found there (channels.json, users.json)",
        })));
    }
    Ok(export)
}

fn admin_group<'a>(groups: &'a [GroupWithChannels], id: &str) -> Result<&'a GroupWithChannels> {
    let g = groups
        .iter()
        .find(|g| g.id == id)
        .ok_or_else(|| Error::Other(anyhow::anyhow!(groups::authz::NOT_A_MEMBER)))?;
    if g.current_user_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can import into a group")));
    }
    Ok(g)
}

/// The group as it is now, from the DS.
async fn group_state(user_id: &str, group_id: &str, state: &Arc<AppState>) -> Result<GroupWithChannels> {
    groups::list_user_groups_with_channels(user_id.to_string(), state)
        .await?
        .into_iter()
        .find(|g| g.id == group_id)
        .ok_or_else(|| Error::Other(anyhow::anyhow!(groups::authz::NOT_A_MEMBER)))
}

/// `(lower-cased category name, lower-cased channel name)` for every channel.
fn channel_keys(g: &GroupWithChannels) -> BTreeSet<(Option<String>, String)> {
    let names: BTreeMap<&str, &str> = g.categories.iter().map(|k| (k.id.as_str(), k.name.as_str())).collect();
    g.channels
        .iter()
        .map(|c| {
            let cat = c.category_id.as_deref().and_then(|id| names.get(id)).map(|n| n.to_lowercase());
            (cat, c.name.to_lowercase())
        })
        .collect()
}

/// Plan against what `g` already holds.
async fn plan_against(
    export: &pollis_import::model::Export,
    g: &GroupWithChannels,
    include_archived: bool,
    user_id: &str,
    state: &Arc<AppState>,
) -> Result<ImportPlan> {
    let names: BTreeMap<&str, &str> = g.categories.iter().map(|k| (k.id.as_str(), k.name.as_str())).collect();
    let existing_channels = g
        .channels
        .iter()
        .map(|c| ExistingChannel {
            category: c.category_id.as_deref().and_then(|id| names.get(id)).map(|n| n.to_string()),
            name: c.name.clone(),
        })
        .collect();
    let emoji = crate::commands::emoji::list_group_emoji(g.id.clone(), user_id.to_string(), state).await?;
    let mut by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &emoji {
        by_hash.entry(e.content_hash.clone()).or_default().push(e.shortcode.clone());
    }
    let imported_emoji = match_emoji_by_picture(export, by_hash).await;
    let options = ImportOptions {
        existing_categories: g.categories.iter().map(|k| k.name.clone()).collect(),
        existing_channels,
        existing_shortcodes: emoji.into_iter().map(|e| e.shortcode).collect(),
        imported_emoji,
        include_archived,
    };
    Ok(pollis_import::plan::plan(export, &options))
}

/// Source emoji whose picture the group already holds, re-encoded exactly as
/// an upload would be, so the content hash matches the stored one.
///
/// One picture can sit under several shortcodes (a Slack alias is its own
/// shortcode for its target's picture), so each source emoji claims the
/// shortcode that is its own name, or its name with the collision suffix
/// (`party_parrot_2`). A picture under some other name still counts for an
/// ordinary emoji (an admin renamed it), never for an alias: the alias then
/// gets its own shortcode on the next run.
async fn match_emoji_by_picture(
    export: &pollis_import::model::Export,
    by_hash: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, String> {
    if by_hash.is_empty() {
        return BTreeMap::new();
    }
    let candidates: Vec<(String, String, bool, PathBuf)> = export
        .emoji
        .values()
        .filter_map(|e| match &e.image {
            Some(Media::Local(p)) if p.is_file() => Some((e.id.clone(), e.name.clone(), e.alias_of.is_some(), p.clone())),
            _ => None,
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut hashes: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
        let mut out = BTreeMap::new();
        for (id, name, alias, path) in candidates {
            let hash = hashes
                .entry(path.clone())
                .or_insert_with(|| {
                    let raw = std::fs::read(&path).ok()?;
                    let encoded = crate::commands::emoji::encode_emoji(&raw).ok()?;
                    Some(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&encoded.bytes)))
                })
                .clone();
            let Some(codes) = hash.and_then(|h| by_hash.get(&h)) else {
                continue;
            };
            let own = pollis_import::shortcode::normalise(&name);
            let named = codes.iter().find(|c| {
                c.as_str() == own
                    || c.strip_prefix(&own)
                        .and_then(|rest| rest.strip_prefix('_'))
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            });
            let code = match (named, alias) {
                (Some(c), _) => Some(c.clone()),
                (None, false) => codes.first().cloned(),
                (None, true) => None,
            };
            if let Some(code) = code {
                out.insert(id, code);
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

/// Put the plan's channels in plan order within each block. Returns how
/// many moved.
async fn reorder(plan: &ImportPlan, user_id: &str, group_id: &str, state: &Arc<AppState>) -> Result<u32> {
    let g = group_state(user_id, group_id, state).await?;
    let ids: BTreeMap<String, String> = g.categories.iter().map(|k| (k.name.to_lowercase(), k.id.clone())).collect();
    let cat_names: BTreeMap<&str, String> = g.categories.iter().map(|k| (k.id.as_str(), k.name.to_lowercase())).collect();
    let mut moved = 0;
    let mut blocks: Vec<Option<String>> = vec![None];
    blocks.extend(plan.categories.iter().map(|k| Some(k.name.to_lowercase())));
    for block in blocks {
        let category_id = match &block {
            Some(name) => match ids.get(name) {
                Some(id) => Some(id.clone()),
                None => continue,
            },
            None => None,
        };
        // The block's channels as they stand, top to bottom.
        let mut order: Vec<(String, String)> = g
            .channels
            .iter()
            .filter(|c| c.category_id.as_deref().and_then(|id| cat_names.get(id)).cloned() == block)
            .map(|c| (c.id.clone(), c.name.to_lowercase()))
            .collect();
        let wanted: Vec<String> = plan
            .channels
            .iter()
            .filter(|c| c.category.as_ref().map(|k| k.to_lowercase()) == block)
            .map(|c| c.name.to_lowercase())
            .collect();
        for (i, name) in wanted.iter().enumerate() {
            let Some(at) = order.iter().position(|(_, n)| n == name) else {
                continue;
            };
            if at == i {
                continue;
            }
            let entry = order.remove(at);
            groups::move_channel(
                group_id.to_string(),
                entry.0.clone(),
                category_id.clone(),
                i as u32,
                user_id.to_string(),
                state,
            )
            .await?;
            order.insert(i, entry);
            moved += 1;
        }
    }
    Ok(moved)
}

/// Upload the server icon as the group's icon (`group-icons/<id>/<hash>`),
/// re-encoded as a profile picture is.
async fn set_group_icon(group_id: &str, user_id: &str, path: &Path, state: &Arc<AppState>) -> Result<()> {
    let input = tokio::fs::read(path)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("read icon: {e}")))?;
    let (bytes, content_type) = tokio::task::spawn_blocking(move || crate::commands::avatar::prepare_upload(&input))
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("prepare icon: {e}")))??;
    let uploaded =
        crate::commands::r2::upload_public_file(format!("group-icons/{group_id}"), bytes, content_type.to_string(), state)
            .await?;
    groups::update_group(group_id.to_string(), user_id.to_string(), None, None, Some(uploaded.key), state).await?;
    Ok(())
}

fn failure(kind: &str, name: &str, e: impl std::fmt::Display) -> ImportFailure {
    ImportFailure { kind: kind.to_string(), name: name.to_string(), error: e.to_string() }
}
