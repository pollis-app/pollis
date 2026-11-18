//! Per-job state and the host functions, each checked against the grants.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use pollis_app_manifest::{ActionKind, EffectiveGrants};
use wasmtime::ResourceLimiter;

use crate::bindings::pollis::app::{host, types as wit};
use crate::egress;
use crate::types::*;

/// Every function the host imports into an App. `tests/enforcement.rs`
/// checks this against the WIT and that none of them moderates.
pub const HOST_IMPORTS: [&str; 13] = [
    "post",
    "reply",
    "react",
    "edit-own",
    "delete-own",
    "query-messages",
    "kv-get",
    "kv-set",
    "kv-delete",
    "http-fetch",
    "log",
    "now",
    "random",
];

#[derive(Debug)]
pub struct HostCallLimitHit(pub String);

impl fmt::Display for HostCallLimitHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostCallLimitHit {}

#[derive(Debug)]
pub struct DeadlineHit;

impl fmt::Display for DeadlineHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("job deadline reached")
    }
}

impl std::error::Error for DeadlineHit {}

#[derive(Debug)]
pub struct MemoryCapHit;

impl fmt::Display for MemoryCapHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("memory cap reached")
    }
}

impl std::error::Error for MemoryCapHit {}

/// Caps linear memory across all of the App's memories; growth past the cap
/// traps (rather than returning -1 to the guest).
pub struct Limiter {
    cap: usize,
    used: usize,
    pub exceeded: bool,
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = self.used - current.min(self.used) + desired;
        if total > self.cap {
            self.exceeded = true;
            return Err(wasmtime::Error::new(MemoryCapHit));
        }
        self.used = total;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 100_000)
    }

    fn instances(&self) -> usize {
        64
    }

    fn tables(&self) -> usize {
        64
    }

    fn memories(&self) -> usize {
        4
    }
}

#[derive(Default)]
struct Counters {
    host: u32,
    http: u32,
    queries: u32,
}

struct Kv {
    /// Committed usage, read once on the first state call.
    base_usage: Option<u64>,
    /// This job's writes, applied only on ok.
    writes: BTreeMap<String, Option<Vec<u8>>>,
    /// Usage change from `writes`.
    delta: i64,
}

pub struct JobState {
    ctx: InstallationContext,
    grants: EffectiveGrants,
    limits: JobLimits,
    services: Arc<dyn HostServices>,
    deadline: Instant,
    counters: Counters,
    actions: Vec<Action>,
    kv: Kv,
    /// Messages this job has been shown, by id: the trigger's and query
    /// results. Actions on them need no lookup.
    seen: HashMap<String, MessageMeta>,
    /// The trigger's own message, already delivered, so exempt from the
    /// history window in queries.
    trigger_messages: HashSet<String>,
    pub limiter: Limiter,
}

impl JobState {
    pub fn new(
        ctx: InstallationContext,
        grants: EffectiveGrants,
        limits: JobLimits,
        services: Arc<dyn HostServices>,
        deadline: Instant,
        trigger: &Trigger,
    ) -> JobState {
        let mut seen = HashMap::new();
        let mut trigger_messages = HashSet::new();
        if let Some(m) = crate::trigger_message(trigger) {
            seen.insert(m.id.clone(), meta(m));
            trigger_messages.insert(m.id.clone());
        }
        let limiter = Limiter {
            cap: limits.memory_bytes,
            used: 0,
            exceeded: false,
        };
        JobState {
            ctx,
            grants,
            limits,
            services,
            deadline,
            counters: Counters::default(),
            actions: Vec::new(),
            kv: Kv {
                base_usage: None,
                writes: BTreeMap::new(),
                delta: 0,
            },
            seen,
            trigger_messages,
            limiter,
        }
    }

    pub fn into_outcome(self) -> JobOutcome {
        JobOutcome {
            actions: self.actions,
            kv_writes: self
                .kv
                .writes
                .into_iter()
                .map(|(key, value)| KvWrite { key, value })
                .collect(),
        }
    }

    /// Every host call starts here: the deadline and the total call cap.
    fn enter(&mut self, op: &str) -> wasmtime::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(wasmtime::Error::new(DeadlineHit));
        }
        self.counters.host += 1;
        if self.counters.host > self.limits.max_host_calls {
            return Err(cap_hit(op, "host calls", self.limits.max_host_calls));
        }
        Ok(())
    }

    fn require_action(&self, op: &str, kind: ActionKind) -> Result<(), String> {
        if self.grants.actions.contains(&kind) {
            Ok(())
        } else {
            Err(format!("`{op}` refused: not in `actions`"))
        }
    }

    fn check_text(&self, op: &str, text: &str) -> Result<(), String> {
        if text.len() > self.limits.max_text_bytes {
            Err(format!(
                "`{op}` refused: text exceeds {} bytes",
                self.limits.max_text_bytes
            ))
        } else {
            Ok(())
        }
    }

    fn queue(&mut self, op: &str, action: Action) -> wasmtime::Result<Result<(), String>> {
        if self.actions.len() as u32 >= self.limits.max_actions {
            return Err(cap_hit(op, "actions", self.limits.max_actions));
        }
        self.actions.push(action);
        Ok(Ok(()))
    }

    /// Find a message an action addresses; refuse it unless it is in a
    /// granted channel.
    fn resolve(&mut self, op: &str, id: &str) -> Result<MessageMeta, String> {
        let refused = || format!("`{op}` on `{id}` refused: not a message in `read.channels`");
        let found = match self.seen.get(id) {
            Some(m) => Some(m.clone()),
            None => self
                .services
                .lookup_message(&self.ctx.installation_id, id)
                .map_err(|e| format!("`{op}` failed: {}", e.0))?,
        };
        let meta = found.ok_or_else(refused)?;
        if !self.grants.channels.contains_key(&meta.channel) {
            return Err(refused());
        }
        Ok(meta)
    }

    fn message_action(
        &mut self,
        op: &str,
        kind: ActionKind,
        id: &str,
        text: Option<&str>,
        own_only: bool,
    ) -> Result<MessageMeta, String> {
        self.require_action(op, kind)?;
        if let Some(text) = text {
            self.check_text(op, text)?;
        }
        let meta = self.resolve(op, id)?;
        if own_only && meta.author_id != self.ctx.bot_user_id {
            return Err(format!(
                "`{op}` on `{id}` refused: not a message this bot sent"
            ));
        }
        Ok(meta)
    }

    fn kv_ready(&mut self, op: &str, key: &str) -> Result<u64, String> {
        if self.grants.state_quota == 0 {
            return Err(format!("`{op}` refused: no `state.quota` declared"));
        }
        if key.is_empty() || key.len() > self.limits.max_kv_key_bytes {
            return Err(format!(
                "`{op}` refused: keys are 1 to {} bytes",
                self.limits.max_kv_key_bytes
            ));
        }
        match self.kv.base_usage {
            Some(u) => Ok(u),
            None => {
                let u = self
                    .services
                    .kv_usage(&self.ctx.installation_id)
                    .map_err(|e| format!("`{op}` failed: {}", e.0))?;
                self.kv.base_usage = Some(u);
                Ok(u)
            }
        }
    }

    fn kv_current(&self, op: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        match self.kv.writes.get(key) {
            Some(v) => Ok(v.clone()),
            None => self
                .services
                .kv_get(&self.ctx.installation_id, key)
                .map_err(|e| format!("`{op}` failed: {}", e.0)),
        }
    }

    fn kv_write(&mut self, op: &str, key: String, value: Option<Vec<u8>>) -> Result<(), String> {
        let base = self.kv_ready(op, &key)?;
        let old = self.kv_current(op, &key)?;
        let size = |v: &Option<Vec<u8>>| v.as_ref().map_or(0, |v| (key.len() + v.len()) as i64);
        let delta = self.kv.delta - size(&old) + size(&value);
        let projected = base as i64 + delta;
        if value.is_some() && projected > self.grants.state_quota as i64 {
            return Err(format!(
                "`{op}` refused: would exceed `state.quota` ({} bytes)",
                self.grants.state_quota
            ));
        }
        self.kv.delta = delta;
        self.kv.writes.insert(key, value);
        Ok(())
    }

    fn fetch(
        &mut self,
        request: wit::HttpRequest,
    ) -> wasmtime::Result<Result<wit::HttpResponse, String>> {
        let checked = (|| {
            let method = egress::check_method(&request.method)?;
            egress::check_headers(&request.headers)?;
            let url = egress::check_url(&request.url, &self.grants.egress)?;
            Ok::<_, String>((method, url))
        })();
        let (mut method, checked) = match checked {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        };
        let host = checked.host.clone();
        let mut url = checked.url;
        let mut body = request.body;
        for _ in 0..=self.limits.max_redirects {
            self.counters.http += 1;
            if self.counters.http > self.limits.max_http_fetches {
                return Err(cap_hit(
                    "http-fetch",
                    "HTTP fetches",
                    self.limits.max_http_fetches,
                ));
            }
            let outbound = OutboundRequest {
                method: method.clone(),
                url: url.to_string(),
                headers: request.headers.clone(),
                body: body.clone(),
            };
            let max = self.limits.max_http_response_bytes;
            let response = match self.services.http_fetch(&outbound, max) {
                Ok(r) => r,
                Err(e) => return Ok(Err(format!("`http-fetch` to `{host}` failed: {}", e.0))),
            };
            if Instant::now() >= self.deadline {
                return Err(wasmtime::Error::new(DeadlineHit));
            }
            if response.body.len() > max {
                return Ok(Err(format!(
                    "`http-fetch` to `{host}` refused: response exceeds {max} bytes"
                )));
            }
            let next = match egress::redirect_target(&url, response.status, &response.headers) {
                None => {
                    return Ok(Ok(wit::HttpResponse {
                        status: response.status,
                        headers: response.headers,
                        body: response.body,
                    }))
                }
                Some(Err(e)) => return Ok(Err(e)),
                Some(Ok(next)) => next,
            };
            let next_host = url::Url::parse(&next)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.trim_end_matches('.').to_string()));
            if next_host.as_deref() != Some(host.as_str()) {
                let shown = next_host.unwrap_or_else(|| next.clone());
                return Ok(Err(format!(
                    "`http-fetch` redirect from `{host}` to `{shown}` refused: redirects may not leave the requested host"
                )));
            }
            url = match egress::check_url(&next, &self.grants.egress) {
                Ok(c) => c.url,
                Err(e) => return Ok(Err(e)),
            };
            if response.status == 303 {
                method = "GET".to_string();
                body = None;
            }
        }
        Ok(Err(format!(
            "`http-fetch` to `{host}` refused: more than {} redirects",
            self.limits.max_redirects
        )))
    }

    fn query(&mut self, filter: wit::MessageFilter) -> Result<Vec<wit::Message>, String> {
        let channels: Vec<String> = match filter.channel {
            Some(c) => {
                if !self.grants.channels.contains_key(&c) {
                    return Err(format!(
                        "`query-messages` on `{c}` refused: not in `read.channels`"
                    ));
                }
                vec![c]
            }
            None => self.grants.channels.keys().cloned().collect(),
        };
        let now = self.services.now_ms();
        let window_start = now.saturating_sub(u64::from(self.grants.history_days) * 86_400_000);
        let since = filter.since.unwrap_or(0).max(window_start);
        let limit = filter.limit.min(self.limits.max_query_results);
        if channels.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let query = MessageQuery {
            channels: channels.clone(),
            since,
            until: filter.until,
            limit,
        };
        let found = self
            .services
            .query_messages(&self.ctx.installation_id, &query)
            .map_err(|e| format!("`query-messages` failed: {}", e.0))?;
        // Filter again: the host never relies on the runtime to have narrowed.
        let mut out = Vec::new();
        for m in found {
            let in_window = m.sent_at >= since || self.trigger_messages.contains(&m.id);
            let before_until = filter.until.is_none_or(|u| m.sent_at < u);
            let event_ok = self.grants.events.contains(&crate::event_kind(&m));
            if channels.contains(&m.channel) && in_window && before_until && event_ok {
                self.seen.insert(m.id.clone(), meta(&m));
                out.push(to_wit_message(&m));
                if out.len() as u32 >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }
}

fn cap_hit(op: &str, what: &str, cap: u32) -> wasmtime::Error {
    wasmtime::Error::new(HostCallLimitHit(format!(
        "`{op}` exceeded the per-job limit of {cap} {what}"
    )))
}

fn meta(m: &Message) -> MessageMeta {
    MessageMeta {
        channel: m.channel.clone(),
        author_id: m.author.id.clone(),
    }
}

impl wit::Host for JobState {}

impl host::Host for JobState {
    fn post(&mut self, channel: String, text: String) -> wasmtime::Result<Result<(), String>> {
        self.enter("post")?;
        let checked = self
            .require_action("post", ActionKind::Post)
            .and_then(|_| self.check_text("post", &text))
            .and_then(|_| {
                if self.grants.channels.contains_key(&channel) {
                    Ok(())
                } else {
                    Err(format!(
                        "`post` to `{channel}` refused: not in `read.channels`"
                    ))
                }
            });
        match checked {
            Ok(()) => self.queue("post", Action::Post { channel, text }),
            Err(e) => Ok(Err(e)),
        }
    }

    fn reply(&mut self, message: String, text: String) -> wasmtime::Result<Result<(), String>> {
        self.enter("reply")?;
        match self.message_action("reply", ActionKind::Reply, &message, Some(&text), false) {
            Ok(meta) => self.queue(
                "reply",
                Action::Reply {
                    channel: meta.channel,
                    message,
                    text,
                },
            ),
            Err(e) => Ok(Err(e)),
        }
    }

    fn react(&mut self, message: String, emoji: String) -> wasmtime::Result<Result<(), String>> {
        self.enter("react")?;
        match self.message_action("react", ActionKind::React, &message, Some(&emoji), false) {
            Ok(meta) => self.queue(
                "react",
                Action::React {
                    channel: meta.channel,
                    message,
                    emoji,
                },
            ),
            Err(e) => Ok(Err(e)),
        }
    }

    fn edit_own(&mut self, message: String, text: String) -> wasmtime::Result<Result<(), String>> {
        self.enter("edit-own")?;
        match self.message_action("edit-own", ActionKind::EditOwn, &message, Some(&text), true) {
            Ok(meta) => self.queue(
                "edit-own",
                Action::EditOwn {
                    channel: meta.channel,
                    message,
                    text,
                },
            ),
            Err(e) => Ok(Err(e)),
        }
    }

    fn delete_own(&mut self, message: String) -> wasmtime::Result<Result<(), String>> {
        self.enter("delete-own")?;
        match self.message_action("delete-own", ActionKind::DeleteOwn, &message, None, true) {
            Ok(meta) => self.queue(
                "delete-own",
                Action::DeleteOwn {
                    channel: meta.channel,
                    message,
                },
            ),
            Err(e) => Ok(Err(e)),
        }
    }

    fn query_messages(
        &mut self,
        filter: wit::MessageFilter,
    ) -> wasmtime::Result<Result<Vec<wit::Message>, String>> {
        self.enter("query-messages")?;
        self.counters.queries += 1;
        if self.counters.queries > self.limits.max_queries {
            return Err(cap_hit(
                "query-messages",
                "queries",
                self.limits.max_queries,
            ));
        }
        Ok(self.query(filter))
    }

    fn kv_get(&mut self, key: String) -> wasmtime::Result<Result<Option<Vec<u8>>, String>> {
        self.enter("kv-get")?;
        Ok(self
            .kv_ready("kv-get", &key)
            .and_then(|_| self.kv_current("kv-get", &key)))
    }

    fn kv_set(&mut self, key: String, value: Vec<u8>) -> wasmtime::Result<Result<(), String>> {
        self.enter("kv-set")?;
        Ok(self.kv_write("kv-set", key, Some(value)))
    }

    fn kv_delete(&mut self, key: String) -> wasmtime::Result<Result<(), String>> {
        self.enter("kv-delete")?;
        Ok(self.kv_write("kv-delete", key, None))
    }

    fn http_fetch(
        &mut self,
        request: wit::HttpRequest,
    ) -> wasmtime::Result<Result<wit::HttpResponse, String>> {
        self.enter("http-fetch")?;
        self.fetch(request)
    }

    fn log(&mut self, level: wit::LogLevel, text: String) -> wasmtime::Result<()> {
        self.enter("log")?;
        let mut text = text;
        if text.len() > self.limits.max_log_bytes {
            let mut cut = self.limits.max_log_bytes;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        let level = match level {
            wit::LogLevel::Trace => LogLevel::Trace,
            wit::LogLevel::Debug => LogLevel::Debug,
            wit::LogLevel::Info => LogLevel::Info,
            wit::LogLevel::Warn => LogLevel::Warn,
            wit::LogLevel::Error => LogLevel::Error,
        };
        self.services.log(&self.ctx.installation_id, level, &text);
        Ok(())
    }

    fn now(&mut self) -> wasmtime::Result<u64> {
        self.enter("now")?;
        Ok(self.services.now_ms())
    }

    fn random(&mut self) -> wasmtime::Result<u64> {
        self.enter("random")?;
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|e| wasmtime::Error::msg(format!("random: {e}")))?;
        Ok(u64::from_le_bytes(bytes))
    }
}

pub fn to_wit_message(m: &Message) -> wit::Message {
    wit::Message {
        id: m.id.clone(),
        channel: m.channel.clone(),
        author: to_wit_author(&m.author),
        text: m.text.clone(),
        attachments: m
            .attachments
            .iter()
            .map(|a| wit::Attachment {
                name: a.name.clone(),
                mime_type: a.mime_type.clone(),
                size: a.size,
            })
            .collect(),
        reply_to: m.reply_to.clone(),
        thread: m.thread.clone(),
        sent_at: m.sent_at,
        edited_at: m.edited_at,
    }
}

fn to_wit_author(a: &Author) -> wit::Author {
    wit::Author {
        id: a.id.clone(),
        display_name: a.display_name.clone(),
        is_bot: a.is_bot,
    }
}

pub fn to_wit_command(c: &Command) -> wit::Command {
    wit::Command {
        name: c.name.clone(),
        args: c.args.clone(),
        message: to_wit_message(&c.message),
    }
}

pub fn to_wit_schedule(s: &Schedule) -> wit::Schedule {
    wit::Schedule {
        name: s.name.clone(),
        slot: s.slot,
    }
}

pub fn to_wit_request(r: &Request) -> wit::Request {
    wit::Request {
        id: r.id.clone(),
        requester: to_wit_author(&r.requester),
        body: r.body.clone(),
    }
}

pub fn to_wit_webhook(w: &Webhook) -> wit::Webhook {
    wit::Webhook {
        delivery_id: w.delivery_id.clone(),
        headers: w.headers.clone(),
        body: w.body.clone(),
    }
}
