//! Slash commands in the composer (#2018): what a member's composer offers
//! after `/`, read from the approved manifests of the group's Apps.
//!
//! A command is an ordinary message (`pollis_app_manifest::commands`), so
//! sending one is the normal send path; this module only answers "which
//! commands are there here" and handles the one kind that is not posted: a
//! `request: true` command goes to the bot as a sealed request
//! ([`send_command_request`]) and never into the channel.
//!
//! **Source of approved manifests.** [`approved_app`] is the single place that
//! reads an installation's approved manifest and grants. App approval (#2005)
//! supplies it; until that lands it returns nothing outside the integration
//! harness, so the composer offers no commands.

use std::collections::BTreeMap;
use std::sync::Arc;

use pollis_app_manifest::{effective_grants, AdminGrants, Manifest, TriggerKind};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Error prefix: the bot request transport (#2008) is not available, so a
/// `request: true` command was not sent (and, as always, not posted).
pub const BOT_REQUESTS_UNAVAILABLE: &str = "bot_requests_unavailable";

/// An installation's approved App, as the composer needs it.
#[derive(Debug, Clone)]
pub struct ApprovedApp {
    pub installation_id: String,
    pub bot_user_id: String,
    pub manifest: Manifest,
    /// The admin's grants from the approval (#2005).
    pub grants: AdminGrants,
}

/// One command the composer can offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCommand {
    pub installation_id: String,
    pub bot_user_id: String,
    /// The bot's `@handle`, inserted after the command when it is ambiguous.
    pub bot_username: String,
    /// The App's name from its manifest, shown beside an ambiguous command.
    pub app_name: String,
    /// Without the slash.
    pub command: String,
    pub description: String,
    pub arg_hint: Option<String>,
    /// Delivered to the bot as a sealed request, never posted in the channel.
    pub request: bool,
    /// Channel ids the App is granted: the command is offered only there.
    pub channels: Vec<String>,
    /// Another App in the group declares the same name: the composer shows the
    /// App's name and inserts `@botUsername`, so routing is unambiguous.
    pub needs_target: bool,
}

/// The approved App of the bot `bot_user_id`, if it has one.
///
/// The one seam to App approval: #2005 replaces the body with its
/// members-readable read of the installation's current approved manifest.
pub async fn approved_app(state: &Arc<AppState>, bot_user_id: &str) -> Result<Option<ApprovedApp>> {
    #[cfg(feature = "test-harness")]
    {
        let _ = state;
        Ok(test_hooks::approved(bot_user_id))
    }
    #[cfg(not(feature = "test-harness"))]
    {
        let _ = (state, bot_user_id);
        Ok(None)
    }
}

/// The commands members can pick in `group_id`'s composer: every command of
/// every bot's approved App, only in its granted channels, with a duplicate
/// name across Apps marked `needs_target`. Readable by any member.
pub async fn list_group_commands(state: &Arc<AppState>, group_id: String) -> Result<Vec<GroupCommand>> {
    let me = crate::commands::mls::current_user_id(state).await?;
    let members = crate::commands::groups::get_group_members(group_id, me, state).await?;
    let mut apps = Vec::new();
    for bot in members.into_iter().filter(|m| m.is_bot) {
        let Some(username) = bot.username else {
            continue;
        };
        if let Some(app) = approved_app(state, &bot.user_id).await? {
            apps.push((username, app));
        }
    }
    Ok(commands_of(&apps))
}

/// [`list_group_commands`] over apps already read: pure, so the rules are
/// unit-tested.
pub fn commands_of(apps: &[(String, ApprovedApp)]) -> Vec<GroupCommand> {
    let mut out = Vec::new();
    for (username, app) in apps {
        let grants = effective_grants(&app.manifest, &app.grants);
        let channels: Vec<String> = grants.channels.keys().cloned().collect();
        if channels.is_empty() {
            continue;
        }
        for c in &app.manifest.commands {
            let trigger = if c.request { TriggerKind::Requests } else { TriggerKind::Commands };
            if !grants.triggers.contains(&trigger) {
                continue;
            }
            out.push(GroupCommand {
                installation_id: app.installation_id.clone(),
                bot_user_id: app.bot_user_id.clone(),
                bot_username: username.clone(),
                app_name: app.manifest.name.clone(),
                command: c.name.clone(),
                description: c.description.trim().to_string(),
                arg_hint: c.arg_hint.clone(),
                request: c.request,
                channels: channels.clone(),
                needs_target: false,
            });
        }
    }
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for c in &out {
        *count.entry(c.command.clone()).or_default() += 1;
    }
    for c in &mut out {
        c.needs_target = count[&c.command] > 1;
    }
    out.sort_by(|a, b| a.command.cmp(&b.command).then_with(|| a.app_name.cmp(&b.app_name)));
    out
}

/// Deliver a `request: true` command to its bot as a sealed request.
///
/// The member-to-bot request transport is #2008's. Until it exists this sends
/// nothing and refuses with [`BOT_REQUESTS_UNAVAILABLE`]; it never falls back
/// to posting the command in the channel.
pub async fn send_command_request(
    state: &Arc<AppState>,
    group_id: String,
    channel_id: String,
    bot_user_id: String,
    command: String,
    args: String,
) -> Result<()> {
    let _ = (state, group_id, channel_id, bot_user_id, command, args);
    Err(Error::Other(anyhow::anyhow!(
        "{BOT_REQUESTS_UNAVAILABLE}: this App's command is delivered privately to the bot, which this version cannot do yet; nothing was sent or posted"
    )))
}

/// Integration-harness seam for [`approved_app`] until App approval (#2005).
#[cfg(feature = "test-harness")]
pub mod test_hooks {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::ApprovedApp;

    static APPROVED: Mutex<Option<HashMap<String, ApprovedApp>>> = Mutex::new(None);

    /// Treat `app` as the approved App of its bot.
    pub fn approve(app: ApprovedApp) {
        let mut g = APPROVED.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(HashMap::new).insert(app.bot_user_id.clone(), app);
    }

    /// Forget every approval set by [`approve`].
    pub fn clear() {
        *APPROVED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(super) fn approved(bot_user_id: &str) -> Option<ApprovedApp> {
        APPROVED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(bot_user_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use pollis_app_manifest::ChannelGrant;

    use super::*;

    fn app(bot: &str, name: &str, commands: &str, channels: &[(&str, &str)]) -> (String, ApprovedApp) {
        let manifest = Manifest::from_json(
            format!(
                r#"{{"name":"{name}","version":"1","triggers":["commands","requests"],
                    "read":{{"channels":["*"]}},"commands":[{commands}]}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        let grants = AdminGrants {
            channels: channels.iter().map(|(id, n)| ChannelGrant { id: (*id).into(), name: (*n).into() }).collect(),
            ..Default::default()
        };
        (
            format!("{bot}_bot"),
            ApprovedApp { installation_id: bot.into(), bot_user_id: bot.into(), manifest, grants },
        )
    }

    #[test]
    fn a_name_two_apps_declare_needs_a_target_and_others_do_not() {
        let apps = [
            app("linear", "linear", r#"{"name":"ticket","description":"File in Linear","arg_hint":"<title>"}"#, &[("c1", "support")]),
            app(
                "jira",
                "jira",
                r#"{"name":"ticket","description":"File in Jira"},{"name":"forget-me","description":"Your data","request":true}"#,
                &[("c1", "support"), ("c2", "ops")],
            ),
        ];
        let out = commands_of(&apps);
        let summary: Vec<(&str, &str, bool, bool)> =
            out.iter().map(|c| (c.command.as_str(), c.app_name.as_str(), c.needs_target, c.request)).collect();
        assert_eq!(
            summary,
            vec![("forget-me", "jira", false, true), ("ticket", "jira", true, false), ("ticket", "linear", true, false)]
        );
        assert_eq!(out[2].bot_username, "linear_bot");
        assert_eq!(out[2].arg_hint.as_deref(), Some("<title>"));
        assert_eq!(out[1].channels, vec!["c1".to_string(), "c2".into()]);
    }

    #[test]
    fn an_app_without_granted_channels_offers_nothing() {
        let apps = [app("linear", "linear", r#"{"name":"ticket","description":"d"}"#, &[])];
        assert!(commands_of(&apps).is_empty());
    }
}
