//! The fake core shared by the job-loop and runtime tests. Several "hosts" of
//! one bot share a [`FakeDs`] (claims + posts), as real hosts share the
//! Delivery Service.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pollis_bot::action::{Action, ChannelInfo};
use pollis_bot::core::{CoreError, InstallationCore, Member, StateCommit, StateValue, StateWrite};
use pollis_bot::handler::{Handler, HandlerError};
use pollis_bot::trigger::{IncomingMessage, Trigger, TriggerKey, WebhookCall};
use pollis_core::commands::bots::app_approval::{
    AppApproval, AppApprovalRecord, AppApprovalRefusal, VerifiedAppApproval, APP_APPROVAL_VERSION,
};

pub const BOT: &str = "01BOTUSER";
pub const CHANNEL: &str = "01ALERTSCHANNEL";

/// The published alerts App (`apps/alerts`), exactly as built: `alerts_app.rs`
/// pins it to `apps/alerts/APP_HASH`.
pub const ALERTS_APP_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alerts-app");

pub fn alerts_app_component() -> Vec<u8> {
    std::fs::read(format!("{ALERTS_APP_DIR}/app.wasm")).unwrap()
}

pub fn alerts_app_manifest() -> Vec<u8> {
    std::fs::read(format!("{ALERTS_APP_DIR}/pollis-app.json")).unwrap()
}

/// What the real core's verification answers once an admin approved the alerts
/// App with `#alerts` granted.
pub fn alerts_approval() -> VerifiedAppApproval {
    let manifest_json = alerts_app_manifest();
    let hash = pollis_app_manifest::app_hash(&alerts_app_component(), &manifest_json).unwrap();
    let grants = pollis_app_manifest::AdminGrants {
        channels: vec![pollis_app_manifest::ChannelGrant { id: CHANNEL.into(), name: "alerts".into() }],
        events: [pollis_app_manifest::EventKind::Created].into(),
        actions: [pollis_app_manifest::ActionKind::Post].into(),
        ..Default::default()
    };
    let record = AppApprovalRecord::Production(AppApproval {
        app_hash: pollis_app_manifest::hex(&hash),
        approver_device_id: "ADMIN-D".into(),
        approver_user_id: "ADMIN".into(),
        auto_update: false,
        bot_user_id: BOT.into(),
        grants,
        group_id: "GROUP".into(),
        installation_id: BOT.into(),
        issued_at: 1_700_000_000,
        manifest_hash: pollis_app_manifest::hex(&pollis_app_manifest::manifest_hash(&manifest_json).unwrap()),
        seq: 1,
        source: None,
        v: APP_APPROVAL_VERSION,
    });
    VerifiedAppApproval {
        record,
        manifest: pollis_app_manifest::Manifest::from_json(&manifest_json).unwrap(),
        manifest_json: String::from_utf8(manifest_json).unwrap(),
    }
}

/// What the DS holds: who claimed each trigger, and the posted messages (idempotent by id).
#[derive(Default)]
pub struct FakeDs {
    pub claims: Mutex<HashMap<(String, String), String>>,
    pub posts: Mutex<Vec<(String, String, String)>>,
    /// The group's message log every host catches up from.
    pub log: Mutex<Vec<IncomingMessage>>,
    /// #2004: App state, key -> (version, value), and the claims whose state was committed.
    pub state: Mutex<HashMap<String, (i64, Vec<u8>)>>,
    pub state_committed: Mutex<Vec<String>>,
    /// The group's roster.
    pub members: Mutex<Vec<Member>>,
}

impl FakeDs {
    pub fn posts(&self) -> Vec<(String, String, String)> {
        self.posts.lock().unwrap().clone()
    }

    pub fn deliver(&self, envelope_id: &str, sender: &str, text: &str) {
        self.log.lock().unwrap().push(IncomingMessage {
            envelope_id: envelope_id.into(),
            channel_id: CHANNEL.into(),
            sender_id: sender.into(),
            text: Some(text.into()),
            reply_to_id: None,
            thread_id: None,
            sent_at: String::new(),
        });
    }
}

pub struct FakeCore {
    /// The admin's approval this host verified, if any.
    pub approval: Mutex<Option<VerifiedAppApproval>>,
    pub device: String,
    pub ds: Arc<FakeDs>,
    /// How far this host has handled the log.
    pub cursor: Mutex<usize>,
    pub active: AtomicUsize,
    pub max_active: AtomicUsize,
    pub catch_up_delay: Duration,
    pub fail_claim: AtomicBool,
    pub fail_post: AtomicBool,
    pub claim_calls: AtomicUsize,
    /// #2004: this host's schedule cursors.
    pub schedule_cursors: Mutex<HashMap<String, u64>>,
    /// Unsealed right now; every other call asserts it.
    pub open: AtomicBool,
    pub opens: AtomicUsize,
    pub closes: AtomicUsize,
    pub fail_open: AtomicBool,
    /// What `host_approvals` answers, and the `wrap_now` flags it was called with.
    pub approvals_retry_at: Mutex<Option<i64>>,
    pub approval_calls: Mutex<Vec<bool>>,
}

impl FakeCore {
    pub fn new(device: &str, ds: Arc<FakeDs>) -> Arc<Self> {
        Arc::new(Self {
            approval: Mutex::new(None),
            device: device.into(),
            ds,
            cursor: Mutex::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            catch_up_delay: Duration::from_millis(0),
            fail_claim: AtomicBool::new(false),
            fail_post: AtomicBool::new(false),
            claim_calls: AtomicUsize::new(0),
            schedule_cursors: Mutex::new(HashMap::new()),
            open: AtomicBool::new(false),
            opens: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            fail_open: AtomicBool::new(false),
            approvals_retry_at: Mutex::new(None),
            approval_calls: Mutex::new(Vec::new()),
        })
    }

    fn assert_open(&self) {
        assert!(self.open.load(Ordering::SeqCst), "the store was used while sealed");
    }

    pub fn slow(device: &str, ds: Arc<FakeDs>) -> Arc<Self> {
        let mut core = Arc::try_unwrap(Self::new(device, ds)).ok().unwrap();
        core.catch_up_delay = Duration::from_millis(20);
        Arc::new(core)
    }
}

#[async_trait]
impl InstallationCore for FakeCore {
    fn installation_id(&self) -> &str {
        BOT
    }

    async fn app_approval(&self) -> Result<Result<VerifiedAppApproval, AppApprovalRefusal>, CoreError> {
        Ok(self.approval.lock().unwrap().clone().ok_or(AppApprovalRefusal::NotApproved))
    }

    fn bot_user_id(&self) -> &str {
        BOT
    }

    async fn open(&self) -> Result<(), CoreError> {
        if self.fail_open.load(Ordering::SeqCst) {
            return Err(CoreError("unattended_refused: the account no longer exists".into()));
        }
        assert!(!self.open.swap(true, Ordering::SeqCst), "opened twice");
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self) {
        assert!(self.open.swap(false, Ordering::SeqCst), "closed while not open");
        self.closes.fetch_add(1, Ordering::SeqCst);
    }

    async fn host_approvals(&self, wrap_now: bool) -> Result<Option<i64>, CoreError> {
        self.assert_open();
        self.approval_calls.lock().unwrap().push(wrap_now);
        Ok(*self.approvals_retry_at.lock().unwrap())
    }

    async fn catch_up(&self) -> Result<Vec<IncomingMessage>, CoreError> {
        self.assert_open();
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.catch_up_delay).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        let cursor = *self.cursor.lock().unwrap();
        Ok(self.ds.log.lock().unwrap()[cursor..].to_vec())
    }

    async fn mark_handled(&self, message: &IncomingMessage) -> Result<(), CoreError> {
        self.assert_open();
        let log = self.ds.log.lock().unwrap();
        let pos = log.iter().position(|m| m.envelope_id == message.envelope_id).expect("in log");
        *self.cursor.lock().unwrap() = pos + 1;
        Ok(())
    }

    async fn channels(&self) -> Result<Vec<ChannelInfo>, CoreError> {
        self.assert_open();
        Ok(vec![
            ChannelInfo { id: CHANNEL.into(), name: "alerts".into(), is_text: true },
            ChannelInfo { id: "01VOICE".into(), name: "standup".into(), is_text: false },
        ])
    }

    async fn claim(&self, key: &TriggerKey) -> Result<bool, CoreError> {
        self.assert_open();
        self.claim_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_claim.load(Ordering::SeqCst) {
            return Err(CoreError("DS unreachable".into()));
        }
        let mut claims = self.ds.claims.lock().unwrap();
        let winner = claims
            .entry((BOT.to_string(), key.as_str().to_string()))
            .or_insert_with(|| self.device.clone());
        Ok(*winner == self.device)
    }

    async fn post(&self, channel_id: &str, text: &str, message_id: &str) -> Result<(), CoreError> {
        self.assert_open();
        if self.fail_post.load(Ordering::SeqCst) {
            return Err(CoreError("DS unreachable".into()));
        }
        let mut posts = self.ds.posts.lock().unwrap();
        if !posts.iter().any(|(id, _, _)| id == message_id) {
            posts.push((message_id.into(), channel_id.into(), text.into()));
        }
        Ok(())
    }

    async fn schedule_cursor(&self, name: &str) -> Result<Option<u64>, CoreError> {
        self.assert_open();
        Ok(self.schedule_cursors.lock().unwrap().get(name).copied())
    }

    async fn mark_schedule(&self, name: &str, slot: u64) -> Result<(), CoreError> {
        self.assert_open();
        self.schedule_cursors.lock().unwrap().insert(name.to_string(), slot);
        Ok(())
    }

    async fn members(&self) -> Result<Vec<Member>, CoreError> {
        self.assert_open();
        Ok(self.ds.members.lock().unwrap().clone())
    }

    async fn state_read(&self, key: &str) -> Result<(StateValue, u64), CoreError> {
        self.assert_open();
        let state = self.ds.state.lock().unwrap();
        let usage = state
            .iter()
            .map(|(k, (_, v))| (k.len() + v.len()) as u64)
            .sum();
        let value = match state.get(key) {
            Some((version, v)) => StateValue {
                version: *version,
                value: Some(v.clone()),
            },
            None => StateValue {
                version: 0,
                value: None,
            },
        };
        Ok((value, usage))
    }

    async fn state_commit(
        &self,
        key: &TriggerKey,
        writes: &[StateWrite],
    ) -> Result<StateCommit, CoreError> {
        self.assert_open();
        let claims = self.ds.claims.lock().unwrap();
        let winner = claims.get(&(BOT.to_string(), key.as_str().to_string()));
        assert_eq!(
            winner,
            Some(&self.device),
            "state committed without this host winning the claim"
        );
        let mut committed = self.ds.state_committed.lock().unwrap();
        if committed.contains(&key.as_str().to_string()) {
            return Ok(StateCommit::AlreadyCommitted);
        }
        let mut state = self.ds.state.lock().unwrap();
        for w in writes {
            let current = state.get(&w.key).map_or(0, |(v, _)| *v);
            if current != w.expected_version {
                return Ok(StateCommit::Conflict);
            }
        }
        for w in writes {
            match &w.value {
                Some(v) => {
                    let next = state.get(&w.key).map_or(1, |(ver, _)| ver + 1);
                    state.insert(w.key.clone(), (next, v.clone()));
                }
                None => {
                    state.remove(&w.key);
                }
            }
        }
        committed.push(key.as_str().to_string());
        Ok(StateCommit::Committed)
    }

    async fn act(
        &self,
        action: &pollis_bot::action::CheckedAction,
        message_id: &str,
    ) -> Result<(), CoreError> {
        use pollis_bot::action::CheckedAction as C;
        match action {
            C::Post { channel_id, text } => self.post(channel_id, text, message_id).await,
            C::Reply {
                channel_id,
                message_id: target,
                text,
            } => {
                self.post(
                    channel_id,
                    &format!("reply to {target}: {text}"),
                    message_id,
                )
                .await
            }
            other => Err(CoreError(format!("fake cannot apply {other:?}"))),
        }
    }
}

/// Replies to every message in its channel: a handler whose actions come from
/// message triggers, which every host sees.
pub struct EchoHandler;

/// A webhook call as a test sender makes it: `{"channel", "text"}`.
pub fn hook_call(delivery: &str, channel: &str, text: &str) -> WebhookCall {
    WebhookCall {
        delivery_id: delivery.into(),
        content_type: Some("application/json".into()),
        headers: Vec::new(),
        body: serde_json::to_vec(&serde_json::json!({ "channel": channel, "text": text })).unwrap(),
    }
}

/// A plain handler for tests of the loop itself: a webhook call's
/// `{"channel", "text"}` becomes one post; anything else does nothing.
pub struct PostHandler;

#[async_trait]
impl Handler for PostHandler {
    fn kind(&self) -> &'static str {
        "post"
    }

    async fn handle(&self, trigger: &Trigger) -> Result<Vec<Action>, HandlerError> {
        let Trigger::Webhook(call) = trigger else {
            return Ok(Vec::new());
        };
        let body: serde_json::Value = serde_json::from_slice(&call.body).map_err(|e| HandlerError(e.to_string()))?;
        Ok(vec![Action::Post {
            channel: body["channel"].as_str().unwrap_or_default().into(),
            text: body["text"].as_str().unwrap_or_default().into(),
        }])
    }
}

#[async_trait]
impl Handler for EchoHandler {
    fn kind(&self) -> &'static str {
        "echo"
    }

    async fn handle(&self, trigger: &Trigger) -> Result<Vec<Action>, HandlerError> {
        match trigger {
            Trigger::Message(m) => Ok(vec![Action::Post {
                channel: m.channel_id.clone(),
                text: format!("echo: {}", m.text.clone().unwrap_or_default()),
            }]),
            Trigger::Webhook(_) | Trigger::Schedule(_) => Ok(Vec::new()),
        }
    }
}

/// Returns actions AND fails, to prove a failure discards them.
pub struct FailingHandler;

#[async_trait]
impl Handler for FailingHandler {
    fn kind(&self) -> &'static str {
        "failing"
    }

    async fn handle(&self, _: &Trigger) -> Result<Vec<Action>, HandlerError> {
        Err(HandlerError("boom".into()))
    }
}

/// Two actions, one inside the grants and one outside.
pub struct MixedHandler;

#[async_trait]
impl Handler for MixedHandler {
    fn kind(&self) -> &'static str {
        "mixed"
    }

    async fn handle(&self, _: &Trigger) -> Result<Vec<Action>, HandlerError> {
        Ok(vec![
            Action::Post { channel: "01OTHERGROUPCHANNEL".into(), text: "leak".into() },
            Action::Post { channel: "alerts".into(), text: "ok".into() },
        ])
    }
}


/// A secret slot in memory, shared by every "process" of one test.
#[derive(Default)]
pub struct MemorySlot(pub Mutex<HashMap<String, Vec<u8>>>);

#[async_trait]
impl pollis_bot::secrets::SecretSlot for MemorySlot {
    async fn load(&self, id: &str) -> anyhow::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
        Ok(self.0.lock().unwrap().get(id).cloned().map(zeroize::Zeroizing::new))
    }
    async fn store(&self, id: &str, secret: &[u8]) -> anyhow::Result<()> {
        self.0.lock().unwrap().insert(id.into(), secret.to_vec());
        Ok(())
    }
}
