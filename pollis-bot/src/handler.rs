//! Handlers turn a trigger into actions (#1182, job loop step 4).
//!
//! A handler is pure decision: it gets the trigger and returns [`Action`]s, and
//! has no way to act itself. The loop owns claiming, grant checks and sending,
//! so a handler of any kind is held to the same rules. The built-in
//! [`AlertsHandler`] is the only kind today; #2004 adds the sandboxed WASM App
//! as a second implementation of [`Handler`], with nothing in the loop
//! changing.

use async_trait::async_trait;

use std::sync::Arc;

use crate::action::Action;
use crate::core::{InstallationCore, StateWrite};
use crate::trigger::{Trigger, WebhookPayload};

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

/// Which handler an installation runs, as written in its `installation.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandlerKind {
    #[default]
    Alerts,
    /// #2004: a sandboxed App: its component and `pollis-app.json`. Until
    /// signed approvals (#2005), a development build reads the admin's grants
    /// from the file named by `POLLIS_APP_DEV_GRANTS`; a release build refuses.
    App { component: std::path::PathBuf, manifest: std::path::PathBuf },
}

impl HandlerKind {
    pub fn build(&self) -> anyhow::Result<Arc<dyn Handler>> {
        Ok(match self {
            HandlerKind::Alerts => Arc::new(AlertsHandler),
            HandlerKind::App { component, manifest } => {
                Arc::new(crate::app_handler::AppHandler::load(component, manifest)?)
            }
        })
    }
}

/// The built-in alerts handler: a webhook call becomes one `post`; messages in
/// the group are ignored.
#[derive(Debug, Default, Clone, Copy)]
pub struct AlertsHandler;

#[async_trait]
impl Handler for AlertsHandler {
    fn kind(&self) -> &'static str {
        "alerts"
    }

    async fn handle(&self, trigger: &Trigger) -> Result<Vec<Action>, HandlerError> {
        match trigger {
            Trigger::Webhook(call) => match &call.payload {
                WebhookPayload::Alert { channel, text } => Ok(vec![Action::Post {
                    channel: channel.clone(),
                    text: text.clone(),
                }]),
                // Only an App installation's webhook is raw.
                WebhookPayload::Raw(_) => Ok(Vec::new()),
            },
            Trigger::Message(_) | Trigger::Schedule(_) => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger::{IncomingMessage, WebhookCall};

    #[tokio::test]
    async fn a_webhook_becomes_one_post_and_a_message_becomes_nothing() {
        let hook = Trigger::Webhook(WebhookCall::alert("d", "alerts", "disk full"));
        assert_eq!(
            AlertsHandler.handle(&hook).await.unwrap(),
            vec![Action::Post { channel: "alerts".into(), text: "disk full".into() }]
        );
        let msg = Trigger::Message(IncomingMessage {
            envelope_id: "e".into(),
            channel_id: "c".into(),
            sender_id: "u".into(),
            text: Some("hello bot".into()),
            reply_to_id: None,
            thread_id: None,
            sent_at: String::new(),
        });
        assert!(AlertsHandler.handle(&msg).await.unwrap().is_empty());
    }
}
