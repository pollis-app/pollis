//! Offline App tests through the real App host (#2006).
//!
//! `pollis app test` runs an App's handlers against fixture messages without a
//! group, a DS or the network, but through the very sandbox the runtime uses:
//! [`pollis_app_host::AppHost::run_job`], with its grant checks, its limits and
//! its refusal messages. Only the runtime's services are replaced, by
//! [`FixtureServices`]: state starts from the request, `query-messages` and
//! message lookups read the request's messages, and `http-fetch` answers from
//! the request's HTTP fixtures (never the network). So a refusal in production
//! is the same refusal, word for word, in a test.
//!
//! The wire format is JSON in camelCase, one [`TestRequest`] in and one
//! [`TestResult`] out, so the TypeScript SDK's test helpers can drive it
//! without linking Rust.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pollis_app_host as host;
use pollis_app_host::{AppHost, HostConfig, HostServices, Job, JobError, ServiceError};
use pollis_app_manifest::{AdminGrants, ChannelGrant, Manifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The installation every test job runs as, unless the request names one.
pub const DEFAULT_INSTALLATION: &str = "test-installation";
/// The bot's user id in tests, unless the request names one.
pub const DEFAULT_BOT: &str = "test-bot";
/// The bot's username in tests, unless the request names one.
pub const DEFAULT_BOT_USERNAME: &str = "test_bot";
/// 2027-01-15T08:00:00Z: a fixed clock, so tests are deterministic.
pub const DEFAULT_NOW: u64 = 1_800_000_000_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestRequest {
    /// The built component (`dist/app.wasm`).
    pub component: PathBuf,
    /// The manifest emitted beside it (`dist/pollis-app.json`).
    pub manifest: PathBuf,
    /// The admin's grants. Absent: everything the manifest asks for, with
    /// each requested channel granted under an id equal to its name.
    #[serde(default)]
    pub grants: Option<AdminGrants>,
    #[serde(default)]
    pub installation_id: Option<String>,
    #[serde(default)]
    pub bot_user_id: Option<String>,
    /// The bot's username, for routing a `send` trigger (commands addressed
    /// `@username`, mentions). Default [`DEFAULT_BOT_USERNAME`].
    #[serde(default)]
    pub bot_username: Option<String>,
    pub trigger: WireTrigger,
    /// What `query-messages` and message lookups can find (the host narrows
    /// it to the grants, as it does in production).
    #[serde(default)]
    pub messages: Vec<WireMessage>,
    /// The installation's state before the job.
    #[serde(default)]
    pub state: BTreeMap<String, Vec<u8>>,
    /// Answers for `http-fetch`, matched by method and exact URL.
    #[serde(default)]
    pub http: Vec<HttpFixture>,
    /// Unix milliseconds the host reports as now.
    #[serde(default)]
    pub now: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireAuthor {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireAttachment {
    pub name: String,
    pub mime_type: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireMessage {
    pub id: String,
    pub channel: String,
    pub author: WireAuthor,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<WireAttachment>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub thread: Option<String>,
    pub sent_at: u64,
    #[serde(default)]
    pub edited_at: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WireTrigger {
    /// A member sent this message in a channel: routed as the runtime routes
    /// it (a declared command to `on-command`, else a mention of the bot to
    /// `on-mention` if the App takes mentions, else `on-message`).
    Send(WireMessage),
    Message(WireMessage),
    Mention(WireMessage),
    Command {
        name: String,
        #[serde(default)]
        args: String,
        message: WireMessage,
    },
    Schedule {
        name: String,
        slot: u64,
    },
    Request {
        id: String,
        requester: WireAuthor,
        body: String,
    },
    Webhook {
        delivery_id: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        body: Vec<u8>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpFixture {
    /// Absent: any method.
    #[serde(default)]
    pub method: Option<String>,
    pub url: String,
    #[serde(default = "ok_status")]
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Vec<u8>,
}

fn ok_status() -> u16 {
    200
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum WireAction {
    Post {
        channel: String,
        text: String,
    },
    Reply {
        channel: String,
        message: String,
        text: String,
    },
    React {
        channel: String,
        message: String,
        emoji: String,
    },
    EditOwn {
        channel: String,
        message: String,
        text: String,
    },
    DeleteOwn {
        channel: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireStateWrite {
    pub key: String,
    /// `None` deletes.
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireLog {
    pub level: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireFetch {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireError {
    /// Stable: `not_delivered`, `grants_refused`, `invalid_component`,
    /// `import_refused`, `timeout`, `memory_exceeded`, `host_call_limit`,
    /// `trap`, `handler_failed`, `setup`.
    pub kind: &'static str,
    pub message: String,
}

/// One job's result. On an error, nothing the job queued would have been
/// applied, so `actions` and `stateWrites` are empty.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
    pub actions: Vec<WireAction>,
    pub state_writes: Vec<WireStateWrite>,
    /// The App's `log` calls and its stdout/stderr, in order.
    pub logs: Vec<WireLog>,
    /// Every `http-fetch` that passed the egress checks, in order.
    pub fetches: Vec<WireFetch>,
}

impl TestResult {
    /// A request that could not be run at all.
    pub fn setup(message: String) -> TestResult {
        TestResult::failed("setup", message)
    }

    fn failed(kind: &'static str, message: String) -> TestResult {
        TestResult {
            ok: false,
            error: Some(WireError { kind, message }),
            actions: Vec::new(),
            state_writes: Vec::new(),
            logs: Vec::new(),
            fetches: Vec::new(),
        }
    }
}

/// The runtime's services, answered from a [`TestRequest`].
pub struct FixtureServices {
    installation_id: String,
    state: HashMap<String, Vec<u8>>,
    messages: Vec<host::Message>,
    http: Vec<HttpFixture>,
    now: u64,
    logs: Mutex<Vec<WireLog>>,
    fetches: Mutex<Vec<WireFetch>>,
}

impl HostServices for FixtureServices {
    fn kv_get(&self, installation_id: &str, key: &str) -> Result<Option<Vec<u8>>, ServiceError> {
        if installation_id != self.installation_id {
            return Ok(None);
        }
        Ok(self.state.get(key).cloned())
    }

    fn kv_usage(&self, installation_id: &str) -> Result<u64, ServiceError> {
        if installation_id != self.installation_id {
            return Ok(0);
        }
        Ok(self
            .state
            .iter()
            .map(|(k, v)| (k.len() + v.len()) as u64)
            .sum())
    }

    fn query_messages(
        &self,
        _installation_id: &str,
        query: &host::MessageQuery,
    ) -> Result<Vec<host::Message>, ServiceError> {
        // Filter by the query the host built, as the runtime's log will; the
        // host filters again on its own.
        let mut out: Vec<host::Message> = self
            .messages
            .iter()
            .filter(|m| query.channels.contains(&m.channel))
            .filter(|m| m.sent_at >= query.since)
            .filter(|m| query.until.is_none_or(|u| m.sent_at < u))
            .cloned()
            .collect();
        out.sort_by_key(|m| m.sent_at);
        out.truncate(query.limit as usize);
        Ok(out)
    }

    fn lookup_message(
        &self,
        _installation_id: &str,
        message_id: &str,
    ) -> Result<Option<host::MessageMeta>, ServiceError> {
        Ok(self
            .messages
            .iter()
            .find(|m| m.id == message_id)
            .map(|m| host::MessageMeta {
                channel: m.channel.clone(),
                author_id: m.author.id.clone(),
            }))
    }

    fn http_fetch(
        &self,
        request: &host::OutboundRequest,
        max_body_bytes: usize,
        _timeout: std::time::Duration,
    ) -> Result<host::HttpResponse, ServiceError> {
        if let Ok(mut fetches) = self.fetches.lock() {
            fetches.push(WireFetch {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            });
        }
        let fixture = self.http.iter().find(|f| {
            f.url == request.url
                && f.method
                    .as_deref()
                    .is_none_or(|m| m.eq_ignore_ascii_case(&request.method))
        });
        let Some(fixture) = fixture else {
            return Err(ServiceError(format!(
                "no HTTP fixture for {} {}: offline tests answer only from the test's `http` fixtures",
                request.method, request.url
            )));
        };
        let mut body = fixture.body.clone();
        // The real client stops reading at max + 1, so the host sees an
        // oversized body exactly as it would in production.
        body.truncate(max_body_bytes.saturating_add(1));
        Ok(host::HttpResponse {
            status: fixture.status,
            headers: fixture.headers.clone(),
            body,
        })
    }

    fn log(&self, _installation_id: &str, level: host::LogLevel, text: &str) {
        if let Ok(mut logs) = self.logs.lock() {
            logs.push(WireLog {
                level: level_name(level),
                text: text.to_string(),
            });
        }
    }

    fn now_ms(&self) -> u64 {
        self.now
    }
}

fn level_name(level: host::LogLevel) -> &'static str {
    match level {
        host::LogLevel::Trace => "trace",
        host::LogLevel::Debug => "debug",
        host::LogLevel::Info => "info",
        host::LogLevel::Warn => "warn",
        host::LogLevel::Error => "error",
    }
}

fn author(a: WireAuthor) -> host::Author {
    host::Author {
        id: a.id,
        display_name: a.display_name,
        is_bot: a.is_bot,
    }
}

fn message(m: WireMessage) -> host::Message {
    host::Message {
        id: m.id,
        channel: m.channel,
        author: author(m.author),
        text: m.text,
        attachments: m
            .attachments
            .into_iter()
            .map(|a| host::Attachment {
                name: a.name,
                mime_type: a.mime_type,
                size: a.size,
            })
            .collect(),
        reply_to: m.reply_to,
        thread: m.thread,
        sent_at: m.sent_at,
        edited_at: m.edited_at,
    }
}

/// Whether `text` mentions `username`, by the rule the runtime uses
/// (`pollis_core::commands::bots::mentions_username`, #843): an `@` at the
/// start or after whitespace, then letters, digits, `_`, `-` or `.`, trailing
/// `.`, `-` and `_` dropped, case-insensitive; `@all` names nobody. Mirrored
/// here because this crate does not link pollis-core.
pub fn mentions(text: &str, username: &str) -> bool {
    let want = username.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' || (i > 0 && !chars[i - 1].is_whitespace()) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '-' | '.'))
        {
            j += 1;
        }
        let token: String = chars[i + 1..j].iter().collect::<String>();
        let token = token.trim_end_matches(['.', '-', '_']).to_lowercase();
        if !token.is_empty() && token != "all" && token == want {
            return true;
        }
        i = j.max(i + 1);
    }
    false
}

/// Route a channel message as the runtime's App handler does.
pub fn route(manifest: &Manifest, m: host::Message, username: &str) -> host::Trigger {
    if let Some(cmd) = pollis_app_manifest::route_command(manifest, &m.text, Some(username)) {
        return host::Trigger::Command(host::Command {
            name: cmd.name,
            args: cmd.args,
            message: m,
        });
    }
    if manifest
        .triggers
        .contains(&pollis_app_manifest::TriggerKind::Mentions)
        && mentions(&m.text, username)
    {
        return host::Trigger::Mention(m);
    }
    host::Trigger::Message(m)
}

fn trigger(t: WireTrigger, manifest: &Manifest, username: &str) -> host::Trigger {
    match t {
        WireTrigger::Send(m) => route(manifest, message(m), username),
        WireTrigger::Message(m) => host::Trigger::Message(message(m)),
        WireTrigger::Mention(m) => host::Trigger::Mention(message(m)),
        WireTrigger::Command {
            name,
            args,
            message: m,
        } => host::Trigger::Command(host::Command {
            name,
            args,
            message: message(m),
        }),
        WireTrigger::Schedule { name, slot } => {
            host::Trigger::Schedule(host::Schedule { name, slot })
        }
        WireTrigger::Request {
            id,
            requester,
            body,
        } => host::Trigger::Request(host::Request {
            id,
            requester: author(requester),
            body,
        }),
        WireTrigger::Webhook {
            delivery_id,
            headers,
            body,
        } => host::Trigger::Webhook(host::Webhook {
            delivery_id,
            headers,
            body,
        }),
    }
}

fn action(a: host::Action) -> WireAction {
    match a {
        host::Action::Post { channel, text } => WireAction::Post { channel, text },
        host::Action::Reply {
            channel,
            message,
            text,
        } => WireAction::Reply {
            channel,
            message,
            text,
        },
        host::Action::React {
            channel,
            message,
            emoji,
        } => WireAction::React {
            channel,
            message,
            emoji,
        },
        host::Action::EditOwn {
            channel,
            message,
            text,
        } => WireAction::EditOwn {
            channel,
            message,
            text,
        },
        host::Action::DeleteOwn { channel, message } => WireAction::DeleteOwn { channel, message },
    }
}

fn error_kind(e: &JobError) -> &'static str {
    match e {
        JobError::NotDelivered(_) => "not_delivered",
        JobError::GrantsRefused(_) => "grants_refused",
        JobError::AppHashMismatch(_) => "app_hash_mismatch",
        JobError::InvalidComponent(_) => "invalid_component",
        JobError::ImportRefused(_) => "import_refused",
        JobError::Timeout(_) => "timeout",
        JobError::MemoryExceeded(_) => "memory_exceeded",
        JobError::HostCallLimit(_) => "host_call_limit",
        JobError::Trap(_) => "trap",
        JobError::HandlerFailed(_) => "handler_failed",
    }
}

/// What a test installation is granted when the request does not say:
/// everything the manifest asks for. A named channel `#support` is granted
/// under the id `support`; `*` grants the channels the request's messages and
/// trigger use.
pub fn full_grants(manifest: &Manifest, channel_ids: &[String]) -> AdminGrants {
    let mut channels: Vec<ChannelGrant> = Vec::new();
    let mut push = |name: &str| {
        if !channels.iter().any(|c| c.name == name) {
            channels.push(ChannelGrant {
                id: name.to_string(),
                name: name.to_string(),
            });
        }
    };
    for selector in &manifest.read.channels {
        if selector == "*" {
            for id in channel_ids {
                push(id);
            }
        } else {
            push(selector.trim_start_matches('#'));
        }
    }
    AdminGrants {
        channels,
        history: manifest.read.history,
        events: manifest.read.events.clone(),
        actions: manifest.actions.clone(),
        egress: manifest.egress.clone(),
    }
}

/// The SHA-256 of the component bytes, used as the cache key for this test
/// process. Not the App hash (#2005 defines that over component + manifest).
fn component_key(component: &[u8]) -> [u8; 32] {
    Sha256::digest(component).into()
}

/// A loaded App, compiled once and run for many test jobs.
pub struct TestHost {
    host: AppHost,
}

impl TestHost {
    pub fn new() -> Result<TestHost, String> {
        let host = AppHost::new(HostConfig::default()).map_err(|e| e.to_string())?;
        Ok(TestHost { host })
    }

    /// Run one job through the real host. Never panics on bad input: every
    /// failure is a [`TestResult`] with an error.
    pub fn run(&self, request: TestRequest) -> TestResult {
        let component = match std::fs::read(&request.component) {
            Ok(b) => b,
            Err(e) => {
                return TestResult::failed(
                    "setup",
                    format!("reading {}: {e}", request.component.display()),
                )
            }
        };
        let manifest_bytes = match std::fs::read(&request.manifest) {
            Ok(b) => b,
            Err(e) => {
                return TestResult::failed(
                    "setup",
                    format!("reading {}: {e}", request.manifest.display()),
                )
            }
        };
        let manifest = match Manifest::from_json(&manifest_bytes) {
            Ok(m) => m,
            Err(e) => return TestResult::failed("setup", e.to_string()),
        };

        let username = request
            .bot_username
            .clone()
            .unwrap_or_else(|| DEFAULT_BOT_USERNAME.to_string());
        let trigger = trigger(request.trigger, &manifest, &username);
        // The runtime fires only declared schedules.
        if let host::Trigger::Schedule(sched) = &trigger {
            if !manifest.schedules.iter().any(|d| d.name == sched.name) {
                return TestResult::failed(
                    "not_delivered",
                    format!(
                        "not delivered: schedule `{}` is not in `schedules`",
                        sched.name
                    ),
                );
            }
        }
        let messages: Vec<host::Message> = request.messages.into_iter().map(message).collect();
        let mut channel_ids: Vec<String> = messages.iter().map(|m| m.channel.clone()).collect();
        if let Some(m) = host::trigger_message(&trigger) {
            channel_ids.push(m.channel.clone());
        }
        let grants = request
            .grants
            .unwrap_or_else(|| full_grants(&manifest, &channel_ids));
        let installation = host::InstallationContext {
            installation_id: request
                .installation_id
                .unwrap_or_else(|| DEFAULT_INSTALLATION.to_string()),
            bot_user_id: request
                .bot_user_id
                .unwrap_or_else(|| DEFAULT_BOT.to_string()),
        };
        let services = Arc::new(FixtureServices {
            installation_id: installation.installation_id.clone(),
            state: request.state.into_iter().collect(),
            messages,
            http: request.http,
            now: request.now.unwrap_or(DEFAULT_NOW),
            logs: Mutex::new(Vec::new()),
            fetches: Mutex::new(Vec::new()),
        });

        let outcome = self.host.run_job(Job {
            installation: &installation,
            app_hash: component_key(&component),
            component: &component,
            manifest: &manifest,
            admin_grants: &grants,
            trigger,
            services: services.clone(),
        });

        let logs = services.logs.lock().map(|l| l.clone()).unwrap_or_default();
        let fetches = services
            .fetches
            .lock()
            .map(|f| f.clone())
            .unwrap_or_default();
        match outcome {
            Ok(outcome) => TestResult {
                ok: true,
                error: None,
                actions: outcome.actions.into_iter().map(action).collect(),
                state_writes: outcome
                    .kv_writes
                    .into_iter()
                    .map(|w| WireStateWrite {
                        key: w.key,
                        value: w.value,
                    })
                    .collect(),
                logs,
                fetches,
            },
            Err(e) => TestResult {
                ok: false,
                error: Some(WireError {
                    kind: error_kind(&e),
                    message: e.to_string(),
                }),
                actions: Vec::new(),
                state_writes: Vec::new(),
                logs,
                fetches,
            },
        }
    }

    /// Compile and link a component without running it: the load check
    /// `pollis app build` runs, so an App that would be refused at install
    /// is refused at build.
    pub fn check(&self, component: &[u8]) -> Result<(), String> {
        self.host
            .prepare(component_key(component), component)
            .map_err(|e| e.to_string())
    }
}
