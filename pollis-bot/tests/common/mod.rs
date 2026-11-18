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
use pollis_bot::core::{CoreError, InstallationCore};
use pollis_bot::handler::{Handler, HandlerError};
use pollis_bot::trigger::{IncomingMessage, Trigger, TriggerKey};

pub const BOT: &str = "01BOTUSER";
pub const CHANNEL: &str = "01ALERTSCHANNEL";

/// What the DS holds: who claimed each trigger, and the posted messages (idempotent by id).
#[derive(Default)]
pub struct FakeDs {
    pub claims: Mutex<HashMap<(String, String), String>>,
    pub posts: Mutex<Vec<(String, String, String)>>,
    /// The group's message log every host catches up from.
    pub log: Mutex<Vec<IncomingMessage>>,
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
}

impl FakeCore {
    pub fn new(device: &str, ds: Arc<FakeDs>) -> Arc<Self> {
        Arc::new(Self {
            device: device.into(),
            ds,
            cursor: Mutex::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            catch_up_delay: Duration::from_millis(0),
            fail_claim: AtomicBool::new(false),
            fail_post: AtomicBool::new(false),
            claim_calls: AtomicUsize::new(0),
        })
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

    fn bot_user_id(&self) -> &str {
        BOT
    }

    async fn catch_up(&self) -> Result<Vec<IncomingMessage>, CoreError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.catch_up_delay).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        let cursor = *self.cursor.lock().unwrap();
        Ok(self.ds.log.lock().unwrap()[cursor..].to_vec())
    }

    async fn mark_handled(&self, message: &IncomingMessage) -> Result<(), CoreError> {
        let log = self.ds.log.lock().unwrap();
        let pos = log.iter().position(|m| m.envelope_id == message.envelope_id).expect("in log");
        *self.cursor.lock().unwrap() = pos + 1;
        Ok(())
    }

    async fn channels(&self) -> Result<Vec<ChannelInfo>, CoreError> {
        Ok(vec![
            ChannelInfo { id: CHANNEL.into(), name: "alerts".into(), is_text: true },
            ChannelInfo { id: "01VOICE".into(), name: "standup".into(), is_text: false },
        ])
    }

    async fn claim(&self, key: &TriggerKey) -> Result<bool, CoreError> {
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
        if self.fail_post.load(Ordering::SeqCst) {
            return Err(CoreError("DS unreachable".into()));
        }
        let mut posts = self.ds.posts.lock().unwrap();
        if !posts.iter().any(|(id, _, _)| id == message_id) {
            posts.push((message_id.into(), channel_id.into(), text.into()));
        }
        Ok(())
    }
}

/// Replies to every message in its channel: a handler whose actions come from
/// message triggers, which every host sees.
pub struct EchoHandler;

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
            Trigger::Webhook(_) => Ok(Vec::new()),
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

