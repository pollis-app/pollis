//! The App handler (#2004): runs a sandboxed App (`pollis-app-host`) as the
//! installation's handler, inside the ordinary job loop.
//!
//! For each trigger the job loop calls [`AppHandler::handle_job`], which:
//!
//! 1. maps the trigger into the App's terms: a message is a command (#2018,
//!    `pollis_app_manifest::route_command`), a mention or a plain message; a
//!    webhook call is a webhook delivery;
//! 2. runs the App on a blocking thread in a fresh sandbox, with
//!    [`AppServices`] as its host services: App state read from the DS through
//!    the open installation, the reference public-address HTTP client, the
//!    trigger's own message for queries, and the runtime log;
//! 3. returns the App's actions and its state writes, each write carrying the
//!    version the App read, for the loop to commit under the trigger's claim.
//!
//! Nothing is held between jobs: the sandbox, the services and every value
//! read are dropped when the job ends. The compiled component is cached by App
//! hash in the process-wide [`AppHost`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pollis_app_host as host;
use pollis_app_host::{AppHost, HostConfig, HostServices, PublicHttpClient, ServiceError};
use pollis_app_manifest::{route_command, Manifest, TriggerKind};
/// The admin's grants, as #2005 and the tests hand them to [`AppHandler::new`].
pub use pollis_app_manifest::{ActionKind, AdminGrants, ChannelGrant, EventKind};
use sha2::{Digest, Sha256};

use crate::action::Action;
use crate::core::{InstallationCore, Member, StateWrite};
use crate::handler::{Handler, HandlerContext, HandlerError, HandlerOutput};
use crate::trigger::{IncomingMessage, Trigger, WebhookPayload};

static COMPILED_CACHE_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Where compiled App components are kept between processes
/// (`<data dir>/app-cache`), so a cold start (`run --once`) loads an App in
/// milliseconds instead of compiling it. Set once, before the first job; a
/// runtime that never sets it compiles in memory only.
pub fn set_compiled_cache_dir(dir: std::path::PathBuf) {
    let _ = COMPILED_CACHE_DIR.set(dir);
}

/// One sandbox host per process, so compiled components are shared by every
/// installation running the same App.
fn app_host() -> Result<Arc<AppHost>, HandlerError> {
    static HOST: OnceLock<Arc<AppHost>> = OnceLock::new();
    if let Some(h) = HOST.get() {
        return Ok(h.clone());
    }
    let config = HostConfig {
        compiled_cache_dir: COMPILED_CACHE_DIR.get().cloned(),
        ..HostConfig::default()
    };
    let h = Arc::new(AppHost::new(config).map_err(|e| HandlerError(e.to_string()))?);
    Ok(HOST.get_or_init(|| h).clone())
}

pub struct AppHandler {
    app_hash: [u8; 32],
    component: Arc<Vec<u8>>,
    manifest: Manifest,
    grants: AdminGrants,
    /// Whether the effective grants deliver `mentions` (a mention of the bot
    /// then reaches `on-mention` instead of `on-message`).
    mentions: bool,
    /// The manifest's schedules, when the effective grants deliver `schedule`.
    schedules: Vec<pollis_app_manifest::ScheduleSpec>,
    http: PublicHttpClient,
}

impl AppHandler {
    /// An App with the admin's grants given directly. #2005 calls this with
    /// grants from a verified approval; tests call it with their own.
    pub fn new(
        component: Vec<u8>,
        manifest_json: &[u8],
        grants: AdminGrants,
    ) -> Result<AppHandler> {
        let manifest = Manifest::from_json(manifest_json)?;
        grants.check_not_wider(&manifest)?;
        // The cache key. #2005 replaces it with the App hash an approval pins.
        let mut h = Sha256::new();
        h.update(b"pollis-app-cache-v1\0");
        h.update((component.len() as u64).to_be_bytes());
        h.update(&component);
        h.update(manifest_json);
        let triggers = pollis_app_manifest::effective_grants(&manifest, &grants).triggers;
        let mentions = triggers.contains(&TriggerKind::Mentions);
        let schedules = if triggers.contains(&TriggerKind::Schedule) {
            manifest.schedules.clone()
        } else {
            Vec::new()
        };
        Ok(AppHandler {
            mentions,
            schedules,
            app_hash: h.finalize().into(),
            component: Arc::new(component),
            manifest,
            grants,
            http: PublicHttpClient::new(),
        })
    }

    /// From an installation's files. Until signed approvals land (#2005) the
    /// admin's grants come from the development grants file, which exists only
    /// in debug builds.
    pub fn load(component: &Path, manifest: &Path) -> Result<AppHandler> {
        let bytes = std::fs::read(component)
            .with_context(|| format!("reading the App component {}", component.display()))?;
        let manifest_json =
            std::fs::read(manifest).with_context(|| format!("reading {}", manifest.display()))?;
        AppHandler::new(bytes, &manifest_json, dev_grants()?)
    }
}

#[cfg(debug_assertions)]
fn dev_grants() -> Result<AdminGrants> {
    use pollis_app_manifest::dev_grants::{load, DEV_GRANTS_ENV};
    let path = std::env::var_os(DEV_GRANTS_ENV).with_context(|| {
        format!("an App installation needs the admin's grants. Fix: until App approvals ship (#2005), set {DEV_GRANTS_ENV} to a grants JSON file (development builds only)")
    })?;
    Ok(load(Path::new(&path))?)
}

#[cfg(not(debug_assertions))]
fn dev_grants() -> Result<AdminGrants> {
    anyhow::bail!("an App installation needs an admin's signed approval, which this release cannot read yet (#2005); release builds never read development grants")
}

/// The App's view of a message the runtime decrypted, with its author from
/// the group's roster (an author no longer in the group is shown by id).
fn to_app_message(m: &IncomingMessage, text: &str, members: &[Member]) -> host::Message {
    let author = members.iter().find(|x| x.user_id == m.sender_id);
    let ms = |t: &str| {
        chrono::DateTime::parse_from_rfc3339(t)
            .map(|d| d.timestamp_millis().max(0) as u64)
            .unwrap_or(0)
    };
    host::Message {
        id: m.envelope_id.clone(),
        channel: m.channel_id.clone(),
        author: host::Author {
            id: m.sender_id.clone(),
            display_name: author
                .and_then(|a| a.username.clone())
                .unwrap_or_else(|| m.sender_id.clone()),
            is_bot: author.is_some_and(|a| a.is_bot),
        },
        text: text.to_string(),
        attachments: Vec::new(),
        reply_to: m.reply_to_id.clone(),
        thread: m.thread_id.clone(),
        sent_at: ms(&m.sent_at),
        edited_at: None,
    }
}

fn to_action(a: host::Action) -> Action {
    match a {
        host::Action::Post { channel, text } => Action::Post { channel, text },
        host::Action::Reply {
            channel,
            message,
            text,
        } => Action::Reply {
            channel,
            message,
            text,
        },
        host::Action::React {
            channel,
            message,
            emoji,
        } => Action::React {
            channel,
            message,
            emoji,
        },
        host::Action::EditOwn {
            channel,
            message,
            text,
        } => Action::EditOwn {
            channel,
            message,
            text,
        },
        host::Action::DeleteOwn { channel, message } => Action::DeleteOwn { channel, message },
    }
}

#[async_trait]
impl Handler for AppHandler {
    fn kind(&self) -> &'static str {
        "app"
    }

    async fn handle(&self, _trigger: &Trigger) -> Result<Vec<Action>, HandlerError> {
        Err(HandlerError(
            "an App runs only through the job loop's handle_job".into(),
        ))
    }

    fn schedules(&self) -> Vec<pollis_app_manifest::ScheduleSpec> {
        self.schedules.clone()
    }

    async fn handle_job(
        &self,
        trigger: &Trigger,
        ctx: &HandlerContext,
    ) -> Result<HandlerOutput, HandlerError> {
        let (app_trigger, message) = match trigger {
            Trigger::Message(m) => {
                // A message this host could not decrypt is nothing an App can read.
                let Some(text) = &m.text else {
                    return Ok(HandlerOutput::default());
                };
                let members = ctx.core.members().await.map_err(|e| HandlerError(e.0))?;
                let msg = to_app_message(m, text, &members);
                let own_name = members
                    .iter()
                    .find(|x| x.user_id == ctx.core.bot_user_id())
                    .and_then(|x| x.username.clone());
                let mentioned = own_name
                    .as_deref()
                    .is_some_and(|n| pollis_core::commands::bots::mentions_username(text, n));
                // One delivery per message, decided here and never by the App:
                // a command declared in the approved manifest (#2018) reaches
                // `on-command`; else a mention of the bot reaches `on-mention`
                // when the App takes mentions; else `on-message`. The sandbox
                // host still refuses any of them outside the granted channels.
                let trigger = if let Some(cmd) = route_command(&self.manifest, text, own_name.as_deref()) {
                    host::Trigger::Command(host::Command { name: cmd.name, args: cmd.args, message: msg.clone() })
                } else if mentioned && self.mentions {
                    host::Trigger::Mention(msg.clone())
                } else {
                    host::Trigger::Message(msg.clone())
                };
                (trigger, Some(msg))
            }
            Trigger::Schedule(slot) => (
                host::Trigger::Schedule(host::Schedule {
                    name: slot.name.clone(),
                    slot: slot.slot.saturating_mul(1000),
                }),
                None,
            ),
            Trigger::Webhook(w) => {
                let hook = match &w.payload {
                    WebhookPayload::Raw(raw) => {
                        let mut headers = raw.headers.clone();
                        if let Some(ct) = &raw.content_type {
                            headers.insert(0, ("content-type".to_string(), ct.clone()));
                        }
                        host::Webhook { delivery_id: w.delivery_id.clone(), headers, body: raw.body.clone() }
                    }
                    // An App installation's listener always sends raw calls;
                    // an alerts-shaped one (`pollis-bot post`) arrives as JSON.
                    WebhookPayload::Alert { channel, text } => host::Webhook {
                        delivery_id: w.delivery_id.clone(),
                        headers: vec![("content-type".to_string(), "application/json".to_string())],
                        body: serde_json::to_vec(&serde_json::json!({ "channel": channel, "text": text }))
                            .map_err(|e| HandlerError(e.to_string()))?,
                    },
                };
                (host::Trigger::Webhook(hook), None)
            }
        };
        let services = Arc::new(AppServices {
            rt: tokio::runtime::Handle::current(),
            core: ctx.core.clone(),
            message,
            versions: Mutex::new(HashMap::new()),
            http: self.http.clone(),
            scope: crate::log::scope(ctx.core.installation_id()),
        });
        let installation = host::InstallationContext {
            installation_id: ctx.core.installation_id().to_string(),
            bot_user_id: ctx.core.bot_user_id().to_string(),
        };
        let sandbox = app_host()?;
        let (component, manifest, grants, app_hash) = (
            self.component.clone(),
            self.manifest.clone(),
            self.grants.clone(),
            self.app_hash,
        );
        let job_services: Arc<dyn HostServices> = services.clone();
        let result = tokio::task::spawn_blocking(move || {
            sandbox.run_job(host::Job {
                installation: &installation,
                app_hash,
                component: &component,
                manifest: &manifest,
                admin_grants: &grants,
                trigger: app_trigger,
                services: job_services,
            })
        })
        .await
        .map_err(|e| HandlerError(format!("the App's job thread failed: {e}")))?;
        match result {
            Ok(outcome) => {
                let versions = services
                    .versions
                    .lock()
                    .map(|v| v.clone())
                    .unwrap_or_default();
                Ok(HandlerOutput {
                    actions: outcome.actions.into_iter().map(to_action).collect(),
                    state: outcome
                        .kv_writes
                        .into_iter()
                        .map(|w| StateWrite {
                            expected_version: versions.get(&w.key).copied().unwrap_or(0),
                            key: w.key,
                            value: w.value,
                        })
                        .collect(),
                })
            }
            // Outside the grants: never an error, the App simply is not run.
            Err(host::JobError::NotDelivered(_)) => Ok(HandlerOutput::default()),
            Err(e) => Err(HandlerError(e.to_string())),
        }
    }
}

/// The runtime side of the sandbox's host services for one job.
pub struct AppServices {
    rt: tokio::runtime::Handle,
    core: Arc<dyn InstallationCore>,
    /// The trigger's message: before the runtime keeps a message log (#2008),
    /// the only history a query returns.
    message: Option<host::Message>,
    /// The version of every key the App read, for the optimistic commit.
    versions: Mutex<HashMap<String, i64>>,
    http: PublicHttpClient,
    scope: String,
}

impl HostServices for AppServices {
    fn kv_get(&self, _installation_id: &str, key: &str) -> Result<Option<Vec<u8>>, ServiceError> {
        let (value, _) = self
            .rt
            .block_on(self.core.state_read(key))
            .map_err(|e| ServiceError(e.0))?;
        if let Ok(mut v) = self.versions.lock() {
            v.insert(key.to_string(), value.version);
        }
        Ok(value.value)
    }

    fn kv_usage(&self, _installation_id: &str) -> Result<u64, ServiceError> {
        // Any read carries the installation's usage; an empty key reads nothing.
        let (_, usage) = self
            .rt
            .block_on(self.core.state_read(""))
            .map_err(|e| ServiceError(e.0))?;
        Ok(usage)
    }

    fn query_messages(
        &self,
        _installation_id: &str,
        query: &host::MessageQuery,
    ) -> Result<Vec<host::Message>, ServiceError> {
        Ok(self
            .message
            .iter()
            .filter(|m| query.channels.contains(&m.channel))
            .cloned()
            .collect())
    }

    fn lookup_message(
        &self,
        _installation_id: &str,
        message_id: &str,
    ) -> Result<Option<host::MessageMeta>, ServiceError> {
        if let Some(m) = self.message.as_ref().filter(|m| m.id == message_id) {
            return Ok(Some(host::MessageMeta {
                channel: m.channel.clone(),
                author_id: m.author.id.clone(),
            }));
        }
        let found = self
            .rt
            .block_on(self.core.message_meta(message_id))
            .map_err(|e| ServiceError(e.0))?;
        Ok(found.map(|(channel, author_id)| host::MessageMeta { channel, author_id }))
    }

    fn http_fetch(
        &self,
        request: &host::OutboundRequest,
        max_body_bytes: usize,
        timeout: Duration,
    ) -> Result<host::HttpResponse, ServiceError> {
        self.http.fetch(request, max_body_bytes, timeout)
    }

    fn log(&self, _installation_id: &str, level: host::LogLevel, text: &str) {
        let level = match level {
            host::LogLevel::Trace => "trace",
            host::LogLevel::Debug => "debug",
            host::LogLevel::Info => "info",
            host::LogLevel::Warn => "warn",
            host::LogLevel::Error => "error",
        };
        crate::log::info(&self.scope, format!("app {level}: {text}"));
    }
}
