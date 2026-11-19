//! Handlers turn a trigger into actions (#1182, job loop step 4).
//!
//! A handler is pure decision: it gets the trigger and returns [`Action`]s, and
//! has no way to act itself. The loop owns claiming, grant checks and sending,
//! so a handler is held to the same rules whatever it is. The runtime's only
//! handler is the sandboxed App, [`crate::app_handler::AppHandler`]; the trait
//! is what keeps the loop testable without one.

use async_trait::async_trait;

use std::sync::Arc;

use crate::action::Action;
use crate::core::{CoreError, InstallationCore, StateWrite};
use crate::trigger::Trigger;

/// A handler's failure. The loop discards every action of a job whose handler
/// fails, so an error is never half-applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerError(pub String);

impl std::fmt::Display for HandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[async_trait]
pub trait Handler: Send + Sync {
    /// Shown in logs and `pollis-bot status`.
    fn kind(&self) -> &'static str;

    async fn handle(&self, trigger: &Trigger) -> Result<Vec<Action>, HandlerError>;

    /// #2005: called once at the start of every job, with the installation
    /// open, before any trigger. An App re-checks its approval and artifact
    /// here. `Err` stops the job without handling anything (nothing is skipped).
    async fn prepare(&self, _ctx: &HandlerContext) -> Result<(), CoreError> {
        Ok(())
    }

    /// What the job loop calls: the handler's actions and, for an App, the
    /// App state writes to commit under the trigger's claim. The default wraps
    /// [`Handler::handle`] with no state.
    async fn handle_job(&self, trigger: &Trigger, _ctx: &HandlerContext) -> Result<HandlerOutput, HandlerError> {
        Ok(HandlerOutput { actions: self.handle(trigger).await?, state: Vec::new() })
    }

    /// #2004: the schedules this handler fires on (an App's manifest-declared
    /// ones, when its grants include the `schedule` trigger). None by default.
    fn schedules(&self) -> Vec<pollis_app_manifest::ScheduleSpec> {
        Vec::new()
    }
}

/// What a handler may use during a job: the open installation.
pub struct HandlerContext {
    pub core: Arc<dyn InstallationCore>,
}

/// A handler's result for one trigger. Nothing is applied until the loop has
/// won the trigger's claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandlerOutput {
    pub actions: Vec<Action>,
    pub state: Vec<StateWrite>,
}
