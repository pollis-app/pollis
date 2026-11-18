//! Handlers turn a trigger into actions (#1182, job loop step 4).
//!
//! A handler is pure decision: it gets the trigger and returns [`Action`]s, and
//! has no way to act itself. The loop owns claiming, grant checks and sending,
//! so a handler of any kind is held to the same rules. The built-in
//! [`AlertsHandler`] is the only kind today; #2004 adds the sandboxed WASM App
//! as a second implementation of [`Handler`], with nothing in the loop
//! changing.

use async_trait::async_trait;

use crate::action::Action;
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
}

/// Which handler an installation runs, as written in its `installation.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandlerKind {
    #[default]
    Alerts,
}

impl HandlerKind {
    pub fn build(self) -> std::sync::Arc<dyn Handler> {
        match self {
            HandlerKind::Alerts => std::sync::Arc::new(AlertsHandler),
        }
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
            Trigger::Webhook(call) => Ok(vec![Action::Post {
                channel: call.channel.clone(),
                text: call.text.clone(),
            }]),
            Trigger::Message(_) => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger::{IncomingMessage, WebhookCall};

    #[tokio::test]
    async fn a_webhook_becomes_one_post_and_a_message_becomes_nothing() {
        let hook = Trigger::Webhook(WebhookCall {
            delivery_id: "d".into(),
            channel: "alerts".into(),
            text: "disk full".into(),
        });
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
