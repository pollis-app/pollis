//! Run your built App offline, in your tests.
//!
//! Each call runs one handler in the same sandbox the bot runtime uses, with
//! the same permission checks and the same error messages, so a call your
//! manifest does not allow fails in a test exactly as it would in a group.
//! Nothing touches the network: `ctx.fetch` answers from the
//! [`http`](JobOptions::http) fixtures you give, and state starts from the
//! [`state`](JobOptions::state) you give.
//!
//! ```no_run
//! use pollis_app::testing::TestApp;
//!
//! #[test]
//! fn hello_replies() {
//!     let app = TestApp::new();
//!     let result = app.command("hello", "world");
//!     assert_eq!(result.replies, ["Hello, world!"]);
//! }
//! ```
//!
//! `pollis app test` builds the App and runs `cargo test` with this wired up.
//! It needs the `pollis-app-dev` program (see the developer guide). Failures
//! to run the App at all panic, which fails the test that hit them.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 2027-01-15T08:00:00Z, the default clock.
pub const DEFAULT_NOW: u64 = 1_800_000_000_000;

/// A canned answer for `ctx.fetch`, matched by exact URL (and method, if given).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFixture {
    url: String,
    method: Option<String>,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpFixture {
    /// A `200` with an empty body, for any method.
    pub fn new(url: impl Into<String>) -> HttpFixture {
        HttpFixture {
            url: url.into(),
            method: None,
            status: 200,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Answer only this method.
    pub fn method(mut self, method: impl Into<String>) -> HttpFixture {
        self.method = Some(method.into());
        self
    }

    /// The response status.
    pub fn status(mut self, status: u16) -> HttpFixture {
        self.status = status;
        self
    }

    /// Add a response header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> HttpFixture {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The response body: text or bytes.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> HttpFixture {
        self.body = body.into();
        self
    }

    /// The response body, as JSON.
    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> HttpFixture {
        self.body = serde_json::to_vec(value).expect("serializing the fixture's body");
        self
    }
}

/// What the admin granted, to test a narrower installation than the manifest asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    channels: BTreeMap<String, String>,
    history: u32,
    events: Vec<String>,
    actions: Vec<String>,
    egress: Vec<String>,
}

impl Grants {
    /// Nothing granted.
    pub fn new() -> Grants {
        Grants::default()
    }

    /// Grant a channel by name, with its id: `.channel("support", "support")`.
    pub fn channel(mut self, name: impl Into<String>, id: impl Into<String>) -> Grants {
        self.channels.insert(name.into(), id.into());
        self
    }

    /// Days of history.
    pub fn history(mut self, days: u32) -> Grants {
        self.history = days;
        self
    }

    /// Event kinds, as the manifest spells them. Default `["created"]`.
    pub fn events<S: Into<String>>(mut self, events: impl IntoIterator<Item = S>) -> Grants {
        self.events = events.into_iter().map(Into::into).collect();
        self
    }

    /// Actions, as the manifest spells them: `["reply", "edit_own"]`.
    pub fn actions<S: Into<String>>(mut self, actions: impl IntoIterator<Item = S>) -> Grants {
        self.actions = actions.into_iter().map(Into::into).collect();
        self
    }

    /// Hosts `ctx.fetch` may reach.
    pub fn egress<S: Into<String>>(mut self, hosts: impl IntoIterator<Item = S>) -> Grants {
        self.egress = hosts.into_iter().map(Into::into).collect();
        self
    }

    fn wire(&self) -> Value {
        let channels: Vec<Value> = self
            .channels
            .iter()
            .map(|(name, id)| json!({ "id": id, "name": name.trim_start_matches('#') }))
            .collect();
        let events = if self.events.is_empty() {
            vec!["created".to_string()]
        } else {
            self.events.clone()
        };
        json!({
            "channels": channels,
            "history": self.history,
            "events": events,
            "actions": self.actions,
            "egress": self.egress,
        })
    }
}

/// A person, as tests describe one. Default: Alice, not a bot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorInput {
    /// Default `u-alice`.
    pub id: Option<String>,
    /// Default `Alice`.
    pub display_name: Option<String>,
    /// Default false.
    pub is_bot: bool,
}

impl AuthorInput {
    /// Someone with this display name.
    pub fn named(display_name: impl Into<String>) -> AuthorInput {
        AuthorInput {
            display_name: Some(display_name.into()),
            ..AuthorInput::default()
        }
    }

    fn wire(&self) -> Value {
        json!({
            "id": self.id.as_deref().unwrap_or("u-alice"),
            "displayName": self.display_name.as_deref().unwrap_or("Alice"),
            "isBot": self.is_bot,
        })
    }
}

/// A message, as tests describe one. A `&str` converts into one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageInput {
    /// Default: a fresh id.
    pub id: Option<String>,
    /// Channel id. In tests a channel's id is its name: `"support"` for
    /// `#support`. Default: the manifest's first named channel, else `general`.
    pub channel: Option<String>,
    pub author: AuthorInput,
    pub text: String,
    pub reply_to: Option<String>,
    pub thread: Option<String>,
    /// Unix milliseconds. Default: the job's clock.
    pub sent_at: Option<u64>,
    /// Unix milliseconds.
    pub edited_at: Option<u64>,
}

impl MessageInput {
    /// A message with this text.
    pub fn new(text: impl Into<String>) -> MessageInput {
        MessageInput {
            text: text.into(),
            ..MessageInput::default()
        }
    }

    /// In this channel.
    pub fn channel(mut self, channel: impl Into<String>) -> MessageInput {
        self.channel = Some(channel.into());
        self
    }

    /// From this person.
    pub fn author(mut self, author: AuthorInput) -> MessageInput {
        self.author = author;
        self
    }

    /// Sent at this time, Unix milliseconds.
    pub fn sent_at(mut self, ms: u64) -> MessageInput {
        self.sent_at = Some(ms);
        self
    }
}

impl From<&str> for MessageInput {
    fn from(text: &str) -> MessageInput {
        MessageInput::new(text)
    }
}

impl From<String> for MessageInput {
    fn from(text: String) -> MessageInput {
        MessageInput::new(text)
    }
}

/// What surrounds one job. Anything unset falls back to the [`TestOptions`]
/// defaults, then to an empty installation with everything the manifest asks
/// for granted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobOptions {
    state: Option<BTreeMap<String, Vec<u8>>>,
    http: Option<Vec<HttpFixture>>,
    messages: Option<Vec<MessageInput>>,
    grants: Option<Grants>,
    now: Option<u64>,
}

impl JobOptions {
    /// Nothing set.
    pub fn new() -> JobOptions {
        JobOptions::default()
    }

    /// State before the job.
    pub fn state(mut self, key: impl Into<String>, value: impl Into<Vec<u8>>) -> JobOptions {
        self.state
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }

    /// An answer for `ctx.fetch`. A request with no fixture fails, as an
    /// unreachable host would.
    pub fn http(mut self, fixture: HttpFixture) -> JobOptions {
        self.http.get_or_insert_with(Vec::new).push(fixture);
        self
    }

    /// History `ctx.messages()` can return (still limited to what is granted).
    pub fn message(mut self, message: impl Into<MessageInput>) -> JobOptions {
        self.messages
            .get_or_insert_with(Vec::new)
            .push(message.into());
        self
    }

    /// Test a narrower installation than the manifest asks for.
    pub fn grants(mut self, grants: Grants) -> JobOptions {
        self.grants = Some(grants);
        self
    }

    /// The runtime's clock, Unix milliseconds. Default [`DEFAULT_NOW`].
    pub fn now(mut self, ms: u64) -> JobOptions {
        self.now = Some(ms);
        self
    }

    fn over(&self, defaults: &JobOptions) -> JobOptions {
        JobOptions {
            state: self.state.clone().or_else(|| defaults.state.clone()),
            http: self.http.clone().or_else(|| defaults.http.clone()),
            messages: self.messages.clone().or_else(|| defaults.messages.clone()),
            grants: self.grants.clone().or_else(|| defaults.grants.clone()),
            now: self.now.or(defaults.now),
        }
    }
}

/// Where the built App is, and defaults for every job.
#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    /// Default: `$POLLIS_APP_COMPONENT`, else `dist/app.wasm`.
    pub component: Option<PathBuf>,
    /// Default: `$POLLIS_APP_MANIFEST`, else `dist/pollis-app.json`.
    pub manifest: Option<PathBuf>,
    /// The `pollis-app-dev` program. Default: `$POLLIS_APP_DEV_BIN`, else
    /// `pollis-app-dev` on PATH.
    pub bin: Option<PathBuf>,
    /// The bot's username, for `@mentions` and commands addressed
    /// `/name @username`. Default `test_bot`.
    pub bot_username: Option<String>,
    /// Defaults for every job.
    pub job: JobOptions,
}

/// One thing the App queued.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Action {
    Post { channel: String, text: String },
    Reply { channel: String, message: String, text: String },
    React { channel: String, message: String, emoji: String },
    EditOwn { channel: String, message: String, text: String },
    DeleteOwn { channel: String, message: String },
}

/// Why a job failed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JobError {
    /// E.g. `handler_failed`, `not_delivered`, `timeout`, `trap`.
    pub kind: String,
    pub message: String,
}

/// A `post` the App queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub channel: String,
    pub text: String,
}

/// One line of the App's log.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LogLine {
    pub level: String,
    pub text: String,
}

/// A `ctx.fetch` that passed the egress check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// What one handler run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobResult {
    /// The handler returned `Ok`.
    pub ok: bool,
    /// Why the job failed.
    pub error: Option<JobError>,
    /// Everything the App queued, in order. Empty when the job failed: nothing
    /// would be sent.
    pub actions: Vec<Action>,
    /// Texts of `reply` actions.
    pub replies: Vec<String>,
    /// `post` actions.
    pub posts: Vec<Post>,
    /// Emoji of `react` actions.
    pub reactions: Vec<String>,
    /// State written by the job, as text (`None`: deleted).
    pub state: BTreeMap<String, Option<String>>,
    /// Every `ctx.log` line and anything printed.
    pub logs: Vec<LogLine>,
    /// Every `ctx.fetch` that passed the egress check.
    pub fetches: Vec<Fetch>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireStateWrite {
    key: String,
    value: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct WireFetch {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireResult {
    ok: bool,
    error: Option<JobError>,
    actions: Vec<Action>,
    state_writes: Vec<WireStateWrite>,
    logs: Vec<LogLine>,
    fetches: Vec<WireFetch>,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl From<WireResult> for JobResult {
    fn from(wire: WireResult) -> JobResult {
        let mut replies = Vec::new();
        let mut posts = Vec::new();
        let mut reactions = Vec::new();
        for action in &wire.actions {
            match action {
                Action::Reply { text, .. } => replies.push(text.clone()),
                Action::Post { channel, text } => posts.push(Post {
                    channel: channel.clone(),
                    text: text.clone(),
                }),
                Action::React { emoji, .. } => reactions.push(emoji.clone()),
                Action::EditOwn { .. } | Action::DeleteOwn { .. } => {}
            }
        }
        JobResult {
            ok: wire.ok,
            error: wire.error,
            actions: wire.actions,
            replies,
            posts,
            reactions,
            state: wire
                .state_writes
                .into_iter()
                .map(|write| (write.key, write.value.as_deref().map(text)))
                .collect(),
            logs: wire.logs,
            fetches: wire
                .fetches
                .into_iter()
                .map(|fetch| Fetch {
                    method: fetch.method,
                    url: fetch.url,
                    headers: fetch.headers,
                    body: fetch.body.as_deref().map(text),
                })
                .collect(),
        }
    }
}

struct Runner {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A built App, ready to run in tests.
pub struct TestApp {
    component: PathBuf,
    manifest_path: PathBuf,
    manifest: Value,
    bin: PathBuf,
    bot_username: Option<String>,
    defaults: JobOptions,
    default_channel: String,
    ids: AtomicU64,
    runner: Mutex<Option<Runner>>,
}

fn path_option(given: &Option<PathBuf>, env: &str, fallback: &str) -> PathBuf {
    given
        .clone()
        .or_else(|| std::env::var_os(env).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(fallback))
}

/// The first named channel in a manifest, as tests name it.
fn first_channel(manifest: &Value) -> String {
    manifest["read"]["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|channel| *channel != "*")
        .map_or_else(
            || "general".to_string(),
            |channel| channel.trim_start_matches('#').to_string(),
        )
}

impl TestApp {
    /// The App `pollis app test` just built.
    pub fn new() -> TestApp {
        TestApp::with_options(TestOptions::default())
    }

    /// A built App, with options.
    pub fn with_options(options: TestOptions) -> TestApp {
        let component = path_option(&options.component, "POLLIS_APP_COMPONENT", "dist/app.wasm");
        let manifest_path =
            path_option(&options.manifest, "POLLIS_APP_MANIFEST", "dist/pollis-app.json");
        let manifest: Value = std::fs::read(&manifest_path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
            .unwrap_or_else(|e| {
                panic!(
                    "no built App at {}: run `pollis app build` first, or run your tests with `pollis app test` ({e})",
                    manifest_path.display()
                )
            });
        let default_channel = first_channel(&manifest);
        TestApp {
            component,
            manifest_path,
            manifest,
            bin: path_option(&options.bin, "POLLIS_APP_DEV_BIN", "pollis-app-dev"),
            bot_username: options.bot_username,
            defaults: options.job,
            default_channel,
            ids: AtomicU64::new(0),
            runner: Mutex::new(None),
        }
    }

    /// The manifest, as `pollis-app.json` holds it.
    pub fn manifest(&self) -> &Value {
        &self.manifest
    }

    /// Run the next call with these options: `app.with(options).command("hello", "")`.
    pub fn with(&self, options: JobOptions) -> Job<'_> {
        Job { app: self, options }
    }

    fn job(&self) -> Job<'_> {
        self.with(JobOptions::new())
    }

    /// A member sends this message in a channel, and it is routed as the
    /// runtime routes it: a command your manifest declares goes to
    /// `on_command`; else a message that @mentions the bot goes to
    /// `on_mention` (if you take mentions); else `on_message`.
    pub fn send(&self, message: impl Into<MessageInput>) -> JobResult {
        self.job().send(message)
    }

    /// `on_message`, directly.
    pub fn message(&self, message: impl Into<MessageInput>) -> JobResult {
        self.job().message(message)
    }

    /// `on_mention`, directly.
    pub fn mention(&self, message: impl Into<MessageInput>) -> JobResult {
        self.job().mention(message)
    }

    /// A member sends `/name args`, routed like [`send`](TestApp::send): an
    /// undeclared command is an ordinary message.
    pub fn command(&self, name: &str, args: &str) -> JobResult {
        self.job().command(name, args)
    }

    /// A declared schedule fires, at the latest slot at or before the clock.
    pub fn schedule(&self, name: &str) -> JobResult {
        self.job().schedule(name)
    }

    /// A member's request.
    pub fn request(&self, body: &str) -> JobResult {
        self.job().request(body)
    }

    /// A webhook delivery with this body.
    pub fn webhook(&self, body: impl Into<Vec<u8>>) -> JobResult {
        self.job().webhook(body)
    }

    /// A webhook delivery with this value as its JSON body.
    pub fn webhook_json<T: Serialize + ?Sized>(&self, body: &T) -> JobResult {
        self.job().webhook_json(body)
    }

    fn next_id(&self) -> u64 {
        self.ids.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn wire_message(&self, input: &MessageInput, now: u64) -> Value {
        json!({
            "id": input.id.clone().unwrap_or_else(|| format!("m{}", self.next_id())),
            "channel": input.channel.as_deref().unwrap_or(&self.default_channel),
            "author": input.author.wire(),
            "text": input.text,
            "replyTo": input.reply_to,
            "thread": input.thread,
            "sentAt": input.sent_at.unwrap_or(now),
            "editedAt": input.edited_at,
        })
    }

    /// The latest slot of a schedule at or before `at` (ms), as the runtime computes it.
    fn slot_at(&self, name: &str, at: u64) -> u64 {
        let schedules = self.manifest["schedules"].as_array();
        let Some(spec) = schedules
            .into_iter()
            .flatten()
            .find(|schedule| schedule["name"] == name)
        else {
            return at;
        };
        let every = spec["every"].as_u64().unwrap_or(1).max(1);
        let offset = spec["offset"].as_u64().unwrap_or(0);
        let secs = at / 1000;
        let slot = if secs < offset {
            offset
        } else {
            (secs - offset) / every * every + offset
        };
        slot * 1000
    }

    fn start(&self) -> Runner {
        let mut child = Command::new(&self.bin)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "could not start `{}`: {e}. Install it or set POLLIS_APP_DEV_BIN (the developer guide's \"Testing\" section says how)",
                    self.bin.display()
                )
            });
        let stdin = child.stdin.take().expect("the runner's stdin");
        let stdout = BufReader::new(child.stdout.take().expect("the runner's stdout"));
        Runner { child, stdin, stdout }
    }

    fn exchange(&self, request: &Value) -> JobResult {
        // A test that panicked while holding the lock left a usable runner.
        let mut guard = self.runner.lock().unwrap_or_else(|e| e.into_inner());
        let runner = guard.get_or_insert_with(|| self.start());
        let mut line = request.to_string();
        line.push('\n');
        let mut answer = String::new();
        let exchanged = runner
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| runner.stdin.flush())
            .and_then(|()| runner.stdout.read_line(&mut answer));
        match exchanged {
            Ok(n) if n > 0 => {}
            Ok(_) => panic!("`{} serve` exited during a job", self.bin.display()),
            Err(e) => panic!("talking to `{} serve`: {e}", self.bin.display()),
        }
        let wire: WireResult = serde_json::from_str(&answer)
            .unwrap_or_else(|e| panic!("unreadable result from `{} serve`: {e}", self.bin.display()));
        wire.into()
    }
}

impl Default for TestApp {
    fn default() -> TestApp {
        TestApp::new()
    }
}

/// One call to a [`TestApp`], with [`JobOptions`].
pub struct Job<'a> {
    app: &'a TestApp,
    options: JobOptions,
}

impl Job<'_> {
    fn now(&self) -> u64 {
        self.options
            .now
            .or(self.app.defaults.now)
            .unwrap_or(DEFAULT_NOW)
    }

    fn run(&self, trigger: Value) -> JobResult {
        let options = self.options.over(&self.app.defaults);
        let now = options.now.unwrap_or(DEFAULT_NOW);
        let messages: Vec<Value> = options
            .messages
            .unwrap_or_default()
            .iter()
            .map(|message| self.app.wire_message(message, now))
            .collect();
        let http: Vec<Value> = options
            .http
            .unwrap_or_default()
            .iter()
            .map(|fixture| {
                json!({
                    "url": fixture.url,
                    "method": fixture.method,
                    "status": fixture.status,
                    "headers": fixture.headers,
                    "body": fixture.body,
                })
            })
            .collect();
        let mut request = json!({
            "component": absolute(&self.app.component),
            "manifest": absolute(&self.app.manifest_path),
            "trigger": trigger,
            "messages": messages,
            "state": options.state.unwrap_or_default(),
            "http": http,
            "now": now,
        });
        if let Some(grants) = options.grants {
            request["grants"] = grants.wire();
        }
        if let Some(username) = &self.app.bot_username {
            request["botUsername"] = json!(username);
        }
        self.app.exchange(&request)
    }

    /// See [`TestApp::send`].
    pub fn send(&self, message: impl Into<MessageInput>) -> JobResult {
        let message = self.app.wire_message(&message.into(), self.now());
        self.run(json!({ "send": message }))
    }

    /// See [`TestApp::message`].
    pub fn message(&self, message: impl Into<MessageInput>) -> JobResult {
        let message = self.app.wire_message(&message.into(), self.now());
        self.run(json!({ "message": message }))
    }

    /// See [`TestApp::mention`].
    pub fn mention(&self, message: impl Into<MessageInput>) -> JobResult {
        let message = self.app.wire_message(&message.into(), self.now());
        self.run(json!({ "mention": message }))
    }

    /// See [`TestApp::command`].
    pub fn command(&self, name: &str, args: &str) -> JobResult {
        self.command_from(name, args, MessageInput::default())
    }

    /// [`command`](Job::command), carried by this message (its text is replaced).
    pub fn command_from(&self, name: &str, args: &str, mut message: MessageInput) -> JobResult {
        message.text = if args.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {args}")
        };
        self.send(message)
    }

    /// See [`TestApp::schedule`].
    pub fn schedule(&self, name: &str) -> JobResult {
        self.schedule_at(name, self.app.slot_at(name, self.now()))
    }

    /// A declared schedule fires for this slot, Unix milliseconds.
    pub fn schedule_at(&self, name: &str, slot: u64) -> JobResult {
        self.run(json!({ "schedule": { "name": name, "slot": slot } }))
    }

    /// See [`TestApp::request`].
    pub fn request(&self, body: &str) -> JobResult {
        self.request_from(body, AuthorInput::default())
    }

    /// A request from this person.
    pub fn request_from(&self, body: &str, requester: AuthorInput) -> JobResult {
        let id = format!("r{}", self.app.next_id());
        self.run(json!({ "request": { "id": id, "requester": requester.wire(), "body": body } }))
    }

    /// See [`TestApp::webhook`].
    pub fn webhook(&self, body: impl Into<Vec<u8>>) -> JobResult {
        self.webhook_with_headers(body, Vec::new())
    }

    /// See [`TestApp::webhook_json`].
    pub fn webhook_json<T: Serialize + ?Sized>(&self, body: &T) -> JobResult {
        let body = serde_json::to_vec(body).expect("serializing the webhook's body");
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        self.webhook_with_headers(body, headers)
    }

    /// A webhook delivery with this body and these headers.
    pub fn webhook_with_headers(
        &self,
        body: impl Into<Vec<u8>>,
        headers: Vec<(String, String)>,
    ) -> JobResult {
        let id = format!("d{}", self.app.next_id());
        self.run(json!({
            "webhook": { "deliveryId": id, "headers": headers, "body": body.into() }
        }))
    }
}

fn absolute(path: &std::path::Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(manifest: Value) -> TestApp {
        let dir = std::env::temp_dir().join(format!(
            "pollis-app-testing-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pollis-app.json");
        std::fs::write(&path, manifest.to_string()).unwrap();
        TestApp::with_options(TestOptions {
            manifest: Some(path),
            ..TestOptions::default()
        })
    }

    #[test]
    fn the_default_channel_is_the_first_named_one() {
        assert_eq!(first_channel(&json!({ "read": { "channels": ["*", "#support"] } })), "support");
        assert_eq!(first_channel(&json!({ "read": { "channels": ["*"] } })), "general");
    }

    #[test]
    fn a_schedules_default_slot_is_the_latest_at_or_before_the_clock() {
        let app = app(json!({
            "read": { "channels": [] },
            "schedules": [{ "name": "daily", "every": 86400, "offset": 32400 }],
        }));
        assert_eq!(app.slot_at("daily", DEFAULT_NOW), 1_799_917_200_000);
        assert_eq!(app.slot_at("daily", 1_000), 32_400_000);
        assert_eq!(app.slot_at("undeclared", 5_000), 5_000);
    }

    #[test]
    fn job_options_fall_back_field_by_field() {
        let defaults = JobOptions::new().now(5).state("a", "1");
        let merged = JobOptions::new().state("b", "2").over(&defaults);
        assert_eq!(merged.now, Some(5));
        assert_eq!(merged.state.unwrap().len(), 1);
    }

    #[test]
    fn a_result_is_sorted_out_by_kind() {
        let wire: WireResult = serde_json::from_value(json!({
            "ok": true,
            "actions": [
                { "type": "reply", "channel": "c", "message": "m", "text": "hi" },
                { "type": "post", "channel": "c", "text": "yo" },
                { "type": "react", "channel": "c", "message": "m", "emoji": "x" },
                { "type": "editOwn", "channel": "c", "message": "m", "text": "e" },
                { "type": "deleteOwn", "channel": "c", "message": "m" },
            ],
            "stateWrites": [{ "key": "a", "value": [49] }, { "key": "b", "value": null }],
            "logs": [{ "level": "info", "text": "l" }],
            "fetches": [{ "method": "GET", "url": "u", "headers": [], "body": null }],
        }))
        .unwrap();
        let result = JobResult::from(wire);
        assert_eq!(result.replies, ["hi"]);
        assert_eq!(result.posts, [Post { channel: "c".to_string(), text: "yo".to_string() }]);
        assert_eq!(result.reactions, ["x"]);
        assert_eq!(result.actions.len(), 5);
        assert_eq!(result.state["a"].as_deref(), Some("1"));
        assert_eq!(result.state["b"], None);
        assert_eq!(result.fetches[0].body, None);
    }
}
