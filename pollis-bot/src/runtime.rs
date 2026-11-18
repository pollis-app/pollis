//! The runtime: many installations, one job loop each, driven by events.
//!
//! Jobs start from exactly three places, none of them a timer:
//! - [`Runtime::wake_all`] once at start, so a restart catches up on what it missed;
//! - [`Runtime::wake`] from a realtime wake-up for the installation's group
//!   (a new message, a membership change, or a reconnect after which missed
//!   events are not replayed);
//! - a webhook call ([`JobSubmitter`]).
//!
//! Each job runs on its own task; the installation's lock serialises them.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::installations::{check_capacity, OverCapacity};
use crate::job::{Installation, JobError, JobInput, JobReport, TriggerOutcome};
use crate::log;
use crate::status::{InstallationState, StatusBoard};
use crate::trigger::WebhookCall;
use crate::webhook::JobSubmitter;

pub struct Runtime {
    installations: HashMap<String, Arc<Installation>>,
    status: Arc<StatusBoard>,
}

impl Runtime {
    /// Refuses more installations than `max_installations`.
    pub fn new(
        installations: Vec<Arc<Installation>>,
        max_installations: usize,
        status: Arc<StatusBoard>,
    ) -> Result<Arc<Self>, OverCapacity> {
        check_capacity(installations.len(), max_installations)?;
        let installations: HashMap<_, _> =
            installations.into_iter().map(|i| (i.id().to_string(), i)).collect();
        for inst in installations.values() {
            status.set(inst.id(), inst.handler_kind(), InstallationState::Starting, None);
        }
        Ok(Arc::new(Self { installations, status }))
    }

    pub fn installation_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.installations.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Catch every installation up (start of the runtime).
    pub fn wake_all(self: &Arc<Self>) {
        for id in self.installation_ids() {
            self.wake(&id);
        }
    }

    /// Something may have arrived for `installation_id`. Wake-ups that arrive
    /// before the queued job starts catching up fold into it.
    pub fn wake(self: &Arc<Self>, installation_id: &str) {
        let Some(inst) = self.installations.get(installation_id).cloned() else {
            return;
        };
        if !inst.queue_wake() {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let _ = this.run_logged(&inst, JobInput::Wake).await;
        });
    }

    /// Run one job with its state logged and written to the status file.
    pub async fn run_logged(&self, inst: &Installation, input: JobInput) -> Result<JobReport, JobError> {
        let scope = log::scope(inst.id());
        let handler = inst.handler_kind();
        self.status.set(inst.id(), handler, InstallationState::CatchingUp, None);
        let result = inst.run_job(input).await;
        match &result {
            Err(e) => {
                log::failure(&scope, format!("job failed while {e}"), e.fix());
                self.status.set(
                    inst.id(),
                    handler,
                    InstallationState::JobFailed,
                    Some((e.to_string(), e.fix().to_string())),
                );
            }
            Ok(report) => {
                let mut first_error = None;
                for t in &report.triggers {
                    match &t.result {
                        Ok(TriggerOutcome::NoActions) => {}
                        Ok(TriggerOutcome::Applied { posted, refused }) => {
                            log::info(&scope, format!("{} {}: posted {posted}", t.kind, t.key));
                            for r in refused {
                                log::failure(&scope, format!("{} {}: {r}", t.kind, t.key), "send to a text channel of the bot's group");
                            }
                        }
                        Ok(TriggerOutcome::ClaimedElsewhere) => {
                            log::info(&scope, format!("{} {}: handled by another host of this bot", t.kind, t.key));
                        }
                        Ok(TriggerOutcome::HandlerFailed { error }) => {
                            log::failure(
                                &scope,
                                format!("{} {}: handler failed, its actions were discarded: {error}", t.kind, t.key),
                                "check the trigger's input; nothing was posted",
                            );
                        }
                        Err(e) => {
                            log::failure(&scope, format!("{} {}: {e}", t.kind, t.key), e.fix());
                            first_error.get_or_insert_with(|| (e.to_string(), e.fix().to_string()));
                        }
                    }
                }
                match first_error {
                    Some(err) => self.status.set(inst.id(), handler, InstallationState::JobFailed, Some(err)),
                    None => self.status.set(inst.id(), handler, InstallationState::Connected, None),
                }
            }
        }
        result
    }
}

impl JobSubmitter for Arc<Runtime> {
    fn submit_webhook(
        &self,
        installation_id: &str,
        call: WebhookCall,
    ) -> oneshot::Receiver<Result<JobReport, JobError>> {
        let (tx, rx) = oneshot::channel();
        if let Some(inst) = self.installations.get(installation_id).cloned() {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                let result = this.run_logged(&inst, JobInput::Webhook(call)).await;
                let _ = tx.send(result);
            });
        }
        // Unknown id: the sender is dropped and the caller sees the runtime as unavailable.
        rx
    }
}
