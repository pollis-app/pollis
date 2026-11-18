//! The job loop (#1182): every capability of the runtime plugs in here.
//!
//! ```text
//! 1. a trigger arrives for an installation (a wake-up for new messages, or a webhook call)
//! 2. take the installation's lock (one job per installation at a time)
//! 3. catch MLS up through the normal ingest path; new messages become message triggers
//! 4. hand each trigger to the installation's handler, which returns actions
//! 5. claim (installation_id, trigger_key) on the DS; a lost claim discards the actions
//! 6. check each action against the grants and apply what passes; a handler error discards all
//! 7. release the lock
//! ```
//!
//! A trigger with no actions is not claimed: there is nothing to deduplicate.
//! A message trigger's cursor advances once its outcome is final (applied,
//! claimed elsewhere, refused or handler failed); a transient failure (the
//! claim or the send could not reach the DS) leaves it unadvanced, so the next
//! wake-up retries it under the same claim key and the same message id.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::action::{CheckedAction, Grants, Refusal};
use crate::core::{CoreError, InstallationCore};
use crate::handler::Handler;
use crate::trigger::{Trigger, TriggerKey, WebhookCall};

/// What woke a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobInput {
    /// Something may have arrived for the group (a realtime wake-up, a
    /// reconnect, or the start of the runtime). The job's catch-up finds out.
    Wake,
    Webhook(WebhookCall),
}

/// How one trigger ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TriggerOutcome {
    /// The handler asked for nothing.
    NoActions,
    /// This host won the claim. `posted` actions were applied; `refused` were
    /// outside the grants and never applied.
    Applied { posted: usize, refused: Vec<Refusal> },
    /// Another host of the bot claimed it first; this host's actions were discarded.
    ClaimedElsewhere,
    /// The handler failed; every action it would have taken was discarded.
    HandlerFailed { error: String },
}

/// A job that could not finish. Each names one concrete fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    CatchUp(CoreError),
    Channels(CoreError),
    Claim(CoreError),
    Post(CoreError),
    MarkHandled(CoreError),
}

impl JobError {
    pub fn fix(&self) -> &'static str {
        match self {
            JobError::CatchUp(_) => {
                "check this host can reach the Delivery Service; the next message or restart retries the catch-up"
            }
            JobError::Channels(_) => {
                "check this host can reach the Delivery Service and the bot is still a member of its group"
            }
            JobError::Claim(_) => {
                "check this host can reach the Delivery Service and has not been revoked (pollis-bot status); the trigger is retried"
            }
            JobError::Post(_) => {
                "check this host can reach the Delivery Service; the message is kept and retried under the same id"
            }
            JobError::MarkHandled(_) => {
                "check the data directory is writable and has free space"
            }
        }
    }
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (step, e) = match self {
            JobError::CatchUp(e) => ("catching up", e),
            JobError::Channels(e) => ("reading the group's channels", e),
            JobError::Claim(e) => ("claiming the trigger", e),
            JobError::Post(e) => ("posting", e),
            JobError::MarkHandled(e) => ("saving progress", e),
        };
        write!(f, "{step}: {e}")
    }
}

/// One trigger's result within a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerResult {
    pub key: TriggerKey,
    pub kind: &'static str,
    pub result: Result<TriggerOutcome, JobError>,
}

/// Everything one job did, for logs, status and the webhook's answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobReport {
    pub triggers: Vec<TriggerResult>,
}

impl JobReport {
    /// The webhook trigger's result, when the job carried one.
    pub fn webhook(&self) -> Option<&TriggerResult> {
        self.triggers.iter().find(|t| t.kind == "webhook")
    }
}

/// One hosted installation: its core, its handler, and its job lock.
pub struct Installation {
    core: Arc<dyn InstallationCore>,
    handler: Arc<dyn Handler>,
    /// Step 2/7: held for a job's whole run.
    lock: Mutex<()>,
    /// A wake job is queued and has not started catching up yet; further
    /// wake-ups fold into it.
    wake_queued: AtomicBool,
}

impl Installation {
    pub fn new(core: Arc<dyn InstallationCore>, handler: Arc<dyn Handler>) -> Self {
        Self { core, handler, lock: Mutex::new(()), wake_queued: AtomicBool::new(false) }
    }

    pub fn id(&self) -> &str {
        self.core.installation_id()
    }

    pub fn handler_kind(&self) -> &'static str {
        self.handler.kind()
    }

    /// Marks a wake job queued. `false` when one already is, and the caller
    /// should not start another (its catch-up has not begun, so it will see
    /// whatever caused this wake-up).
    pub fn queue_wake(&self) -> bool {
        !self.wake_queued.swap(true, Ordering::AcqRel)
    }

    /// Run one job to completion (steps 2–7).
    pub async fn run_job(&self, input: JobInput) -> Result<JobReport, JobError> {
        // Step 2. Dropped at return: step 7.
        let _guard = self.lock.lock().await;
        if input == JobInput::Wake {
            // Wake-ups from here on need a new job: this catch-up may already be past them.
            self.wake_queued.store(false, Ordering::Release);
        }

        // Step 3.
        let messages = self.core.catch_up().await.map_err(JobError::CatchUp)?;

        let mut triggers: Vec<Trigger> = Vec::new();
        // A webhook goes first: its sender is waiting on the answer.
        if let JobInput::Webhook(call) = input {
            triggers.push(Trigger::Webhook(call));
        }
        let own_id = self.core.bot_user_id().to_string();
        triggers.extend(
            messages
                .into_iter()
                .filter(|m| m.sender_id != own_id)
                .map(Trigger::Message),
        );

        let mut report = JobReport::default();
        let mut grants: Option<Grants> = None;
        for trigger in triggers {
            let key = trigger.key();
            let kind = trigger.kind();
            let result = self.run_trigger(&trigger, &mut grants).await;
            let failed = result.is_err();
            if let (Trigger::Message(m), Ok(_)) = (&trigger, &result) {
                self.core.mark_handled(m).await.map_err(JobError::MarkHandled)?;
            }
            report.triggers.push(TriggerResult { key, kind, result });
            // Messages are handled in order: stop at a transient failure so the
            // cursor never skips past one that still needs another attempt.
            if failed && kind == "message" {
                break;
            }
        }
        Ok(report)
    }

    /// Steps 4–6 for one trigger.
    async fn run_trigger(
        &self,
        trigger: &Trigger,
        grants: &mut Option<Grants>,
    ) -> Result<TriggerOutcome, JobError> {
        // Step 4.
        let actions = match self.handler.handle(trigger).await {
            Ok(actions) => actions,
            Err(e) => return Ok(TriggerOutcome::HandlerFailed { error: e.0 }),
        };
        if actions.is_empty() {
            return Ok(TriggerOutcome::NoActions);
        }

        // Step 5.
        let key = trigger.key();
        if !self.core.claim(&key).await.map_err(JobError::Claim)? {
            return Ok(TriggerOutcome::ClaimedElsewhere);
        }

        // Step 6. The channel list is read once per job, and only when needed.
        if grants.is_none() {
            let channels = self.core.channels().await.map_err(JobError::Channels)?;
            *grants = Some(Grants::own_group(channels));
        }
        let grants = grants.as_ref().expect("set just above");
        let mut posted = 0;
        let mut refused = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            match grants.check(action) {
                Ok(CheckedAction::Post { channel_id, text }) => {
                    let message_id = action_message_id(self.id(), trigger, index);
                    self.core
                        .post(&channel_id, &text, &message_id)
                        .await
                        .map_err(JobError::Post)?;
                    posted += 1;
                }
                Err(refusal) => refused.push(refusal),
            }
        }
        Ok(TriggerOutcome::Applied { posted, refused })
    }
}

/// The id of the `index`-th message a trigger's job posts: the same on every
/// run of that trigger, so a retry re-sends the same message rather than a
/// second one. A ULID, as message ids are; its time is the trigger's own ULID
/// time when it has one, else the start of the current UTC day, so a sender
/// retrying a non-ULID delivery id the same day still gets the same id. (The
/// id's time is not the message's `sent_at`; nothing orders by it.)
pub fn action_message_id(installation_id: &str, trigger: &Trigger, index: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"pollis-bot-action-v1\0");
    hasher.update(installation_id.as_bytes());
    hasher.update([0]);
    hasher.update(trigger.key().as_str().as_bytes());
    hasher.update([0]);
    hasher.update((index as u64).to_be_bytes());
    let digest = hasher.finalize();
    let mut random = [0u8; 16];
    random[6..].copy_from_slice(&digest[..10]);
    let ms = match trigger.ulid_hint() {
        Some(u) => u.timestamp_ms(),
        None => {
            const DAY_MS: u64 = 86_400_000;
            let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
            now - now % DAY_MS
        }
    };
    ulid::Ulid::from_parts(ms, u128::from_be_bytes(random)).to_string()
}
