//! A fake runtime for the host tests: in-memory services that record what the
//! host asked of them, and deliberately return MORE than the grants allow, so
//! the tests prove the host narrows on its own.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pollis_app_host::*;
use pollis_app_manifest::{AdminGrants, ChannelGrant, Manifest};

pub const BOT: &str = "u-bot";
pub const NOW: u64 = 1_800_000_000_000;
pub const DAY: u64 = 86_400_000;

pub fn component() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/test-app.wasm"
    ))
    .unwrap()
}

/// Imports wasi:sockets; must be refused.
pub fn net_component() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/net-app.wasm"
    ))
    .unwrap()
}

/// An ordinary std Rust App; must load.
pub fn std_component() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/std-app.wasm"
    ))
    .unwrap()
}

pub const APP_HASH: AppHash = [7u8; 32];

#[derive(Default)]
pub struct Fake {
    /// Committed state, keyed by (installation, key).
    pub kv: Mutex<HashMap<(String, String), Vec<u8>>>,
    /// Everything `query_messages` returns, regardless of the query.
    pub messages: Mutex<Vec<Message>>,
    /// Responses by exact URL.
    pub http: Mutex<HashMap<String, HttpResponse>>,
    pub fetched: Mutex<Vec<OutboundRequest>>,
    pub queries: Mutex<Vec<MessageQuery>>,
    pub logs: Mutex<Vec<(String, String)>>,
}

impl HostServices for Fake {
    fn kv_get(&self, installation_id: &str, key: &str) -> Result<Option<Vec<u8>>, ServiceError> {
        Ok(self
            .kv
            .lock()
            .unwrap()
            .get(&(installation_id.to_string(), key.to_string()))
            .cloned())
    }

    fn kv_usage(&self, installation_id: &str) -> Result<u64, ServiceError> {
        Ok(self
            .kv
            .lock()
            .unwrap()
            .iter()
            .filter(|((i, _), _)| i == installation_id)
            .map(|((_, k), v)| (k.len() + v.len()) as u64)
            .sum())
    }

    fn query_messages(
        &self,
        _installation_id: &str,
        query: &MessageQuery,
    ) -> Result<Vec<Message>, ServiceError> {
        self.queries.lock().unwrap().push(query.clone());
        Ok(self.messages.lock().unwrap().clone())
    }

    fn lookup_message(
        &self,
        _installation_id: &str,
        message_id: &str,
    ) -> Result<Option<MessageMeta>, ServiceError> {
        Ok(self
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.id == message_id)
            .map(|m| MessageMeta {
                channel: m.channel.clone(),
                author_id: m.author.id.clone(),
            }))
    }

    fn http_fetch(
        &self,
        request: &OutboundRequest,
        _max: usize,
        _timeout: std::time::Duration,
    ) -> Result<HttpResponse, ServiceError> {
        self.fetched.lock().unwrap().push(request.clone());
        self.http
            .lock()
            .unwrap()
            .get(&request.url)
            .cloned()
            .ok_or_else(|| ServiceError("no route".into()))
    }

    fn log(&self, installation_id: &str, _level: LogLevel, text: &str) {
        self.logs
            .lock()
            .unwrap()
            .push((installation_id.to_string(), text.to_string()));
    }

    fn now_ms(&self) -> u64 {
        NOW
    }
}

impl Fake {
    pub fn lines(&self) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, t)| t.clone())
            .collect()
    }

    /// What the runtime does with an ok outcome's state writes.
    pub fn commit(&self, installation_id: &str, outcome: &JobOutcome) {
        let mut kv = self.kv.lock().unwrap();
        for w in &outcome.kv_writes {
            let key = (installation_id.to_string(), w.key.clone());
            match &w.value {
                Some(v) => {
                    kv.insert(key, v.clone());
                }
                None => {
                    kv.remove(&key);
                }
            }
        }
    }
}

pub fn manifest() -> Manifest {
    Manifest::from_json(
        br##"{
            "name": "test-app",
            "version": "0.1.0",
            "triggers": ["messages", "mentions", "commands", "schedule", "requests", "webhook"],
            "read": { "channels": ["#general", "#ops"], "history": 7, "events": ["created", "edited"] },
            "actions": ["post", "reply", "react", "edit_own", "delete_own"],
            "state": { "quota": 64 },
            "egress": ["api.x.com"],
            "schedules": [{ "name": "now", "every": 60 }]
        }"##,
    )
    .unwrap()
}

pub fn admin() -> AdminGrants {
    let mut grants = AdminGrants {
        channels: vec![
            ChannelGrant {
                id: "c-general".into(),
                name: "general".into(),
            },
            ChannelGrant {
                id: "c-ops".into(),
                name: "ops".into(),
            },
        ],
        history: 7,
        ..Default::default()
    };
    grants.events = manifest().read.events;
    grants.actions = manifest().actions;
    grants.egress = manifest().egress;
    grants
}

pub fn ctx(installation: &str) -> InstallationContext {
    InstallationContext {
        installation_id: installation.to_string(),
        bot_user_id: BOT.to_string(),
    }
}

pub fn author(id: &str) -> Author {
    Author {
        id: id.to_string(),
        display_name: id.to_string(),
        is_bot: id == BOT,
    }
}

pub fn msg(id: &str, channel: &str, author_id: &str, text: &str, sent_at: u64) -> Message {
    Message {
        id: id.to_string(),
        channel: channel.to_string(),
        author: author(author_id),
        text: text.to_string(),
        attachments: vec![Attachment {
            name: "a.png".into(),
            mime_type: "image/png".into(),
            size: 10,
        }],
        reply_to: None,
        thread: None,
        sent_at,
        edited_at: None,
    }
}

/// A message trigger in #general from a member, carrying `script`.
pub fn on_message(script: &str) -> Trigger {
    Trigger::Message(msg("m1", "c-general", "u-alice", script, NOW - 1000))
}

pub fn host() -> AppHost {
    host_with(JobLimits::default())
}

pub fn host_with(limits: JobLimits) -> AppHost {
    AppHost::new(HostConfig {
        limits,
        cache_capacity: 8,
    })
    .unwrap()
}

pub fn short_deadline() -> JobLimits {
    JobLimits {
        deadline: Duration::from_millis(300),
        ..JobLimits::default()
    }
}

pub struct Run {
    pub result: Result<JobOutcome, JobError>,
    pub fake: Arc<Fake>,
}

impl Run {
    pub fn lines(&self) -> Vec<String> {
        self.fake.lines()
    }

    pub fn ok(&self) -> &JobOutcome {
        self.result
            .as_ref()
            .unwrap_or_else(|e| panic!("job failed: {e}; log: {:?}", self.lines()))
    }

    /// The App's report for a step whose text starts with `prefix`.
    pub fn line(&self, prefix: &str) -> String {
        self.lines()
            .into_iter()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no log line starting {prefix:?}: {:?}", self.lines()))
    }
}

pub fn run_component(
    component: &[u8],
    app_hash: AppHash,
    host: &AppHost,
    fake: Arc<Fake>,
    trigger: Trigger,
) -> Run {
    let ctx = ctx("inst-a");
    let result = host.run_job(Job {
        installation: &ctx,
        app_hash,
        component,
        manifest: &manifest(),
        admin_grants: &admin(),
        trigger,
        services: fake.clone(),
    });
    Run { result, fake }
}

pub fn run_on(
    host: &AppHost,
    fake: Arc<Fake>,
    installation: &str,
    manifest: &Manifest,
    admin: &AdminGrants,
    trigger: Trigger,
) -> Run {
    let component = component();
    let ctx = ctx(installation);
    let result = host.run_job(Job {
        installation: &ctx,
        app_hash: APP_HASH,
        component: &component,
        manifest,
        admin_grants: admin,
        trigger,
        services: fake.clone(),
    });
    Run { result, fake }
}

pub fn run(trigger: Trigger) -> Run {
    run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin(),
        trigger,
    )
}

pub fn run_with(fake: Fake, trigger: Trigger) -> Run {
    run_on(
        &host(),
        Arc::new(fake),
        "inst-a",
        &manifest(),
        &admin(),
        trigger,
    )
}
