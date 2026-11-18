//! The runtime: many installations, one job loop each, driven by events.
//!
//! Jobs start from these places, none of them a periodic timer:
//! - [`Runtime::start_all`] once at start (or [`Runtime::run_once`] for a
//!   one-pass run), so a host that was asleep catches up on what it missed and
//!   handles approvals that arrived meanwhile;
//! - [`Runtime::wake`] from the wake source (a new message or membership change);
//! - [`Runtime::approvals`] from the wake source (an approval nudge, a reconnect)
//!   and, once, at a deferred approval's `retry_at`;
//! - a webhook call ([`JobSubmitter`]).
//!
//! Each job runs on its own task; the installation's lock serialises them, and
//! each one unseals the installation's store at its start and seals it at its end.

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

    /// Catch every installation up (used by tests and by a wake source that lost track).
    pub fn wake_all(self: &Arc<Self>) {
        for id in self.installation_ids() {
            self.wake(&id);
        }
    }

    /// Start of a long-running runtime: every installation catches up and
    /// checks pending approvals.
    pub fn start_all(self: &Arc<Self>) {
        for id in self.installation_ids() {
            self.approvals(&id);
        }
    }

    /// Check `installation_id`'s pending add-host approvals (and catch up). A
    /// request deferred to a peer that ranks first is checked again, once, at
    /// its `retry_at`, so a peer that is asleep never blocks the wrap.
    pub fn approvals(self: &Arc<Self>, installation_id: &str) {
        let Some(inst) = self.installations.get(installation_id).cloned() else {
            return;
        };
        let this = Arc::clone(self);
        let id = installation_id.to_string();
        tokio::spawn(async move {
            let Ok(report) = this.run_logged(&inst, JobInput::Approvals { wrap_now: false }).await else {
                return;
            };
            if let Some(at) = report.approvals_retry_at {
                let wait = (at - chrono::Utc::now().timestamp()).max(1) as u64;
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                this.approvals(&id);
            }
        });
    }

    /// A one-pass run for a host that goes back to sleep afterwards: every
    /// installation catches up, handles what is pending, and wraps approved
    /// hosts at once rather than deferring to a peer. `true` when every job
    /// finished.
    pub async fn run_once(self: &Arc<Self>) -> bool {
        let mut jobs = Vec::new();
        for id in self.installation_ids() {
            let inst = self.installations[&id].clone();
            let this = Arc::clone(self);
            jobs.push(tokio::spawn(async move {
                this.run_logged(&inst, JobInput::Approvals { wrap_now: true }).await.is_ok()
            }));
        }
        let mut all_ok = true;
        for job in jobs {
            all_ok &= job.await.unwrap_or(false);
        }
        all_ok
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
                if let Some(e) = &report.approvals_error {
                    log::failure(
                        &scope,
                        format!("checking host approvals: {e}"),
                        "check this host can reach the Delivery Service; approvals are checked again at the next start, nudge or reconnect",
                    );
                }
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
