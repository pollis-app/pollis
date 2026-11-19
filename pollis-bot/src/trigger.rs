//! What starts a job (#1182, job loop step 1).
//!
//! A trigger is something that happened for one installation. Each has a
//! **trigger key** that every host of the bot derives identically, so the hosts
//! can agree through the DS claim (step 5) on which of them acts.
//!
//! Kinds today: a new message in the bot's group, and a webhook call. #2008 adds
//! member requests (keyed by request id) and schedules (keyed by slot time);
//! they become new variants here and new arms in [`Trigger::key`].

use serde::Serialize;

/// The key a trigger is claimed under, unique per installation.
///
/// Prefixed by kind, so a webhook delivery id a sender chose can never collide
/// with an envelope id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct TriggerKey(String);

impl TriggerKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TriggerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A decrypted message in the bot's group, found by catch-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    /// The envelope (message) id; a ULID, the same on every host.
    pub envelope_id: String,
    pub channel_id: String,
    pub sender_id: String,
    /// `None` for a message this host could not decrypt (#1312).
    pub text: Option<String>,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    pub sent_at: String,
}

/// One authenticated call to `POST /hooks/<installation_id>`: the request as
/// sent, for the App's `on-webhook`, with only the headers it may see (never
/// the credential headers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookCall {
    /// The sender's `X-Pollis-Delivery` header, or a fresh ULID. A sender that
    /// posts the same alert to every host of a bot sends the same id to each,
    /// and the claim lets exactly one of them post it.
    pub delivery_id: String,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// #2004: one slot of an App's manifest-declared schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSlot {
    pub name: String,
    /// Unix seconds; the same on every host of the bot.
    pub slot: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Message(IncomingMessage),
    Webhook(WebhookCall),
    Schedule(ScheduleSlot),
}

impl Trigger {
    /// The envelope id for a message, the delivery id for a webhook.
    pub fn key(&self) -> TriggerKey {
        match self {
            Trigger::Message(m) => TriggerKey(format!("msg:{}", m.envelope_id)),
            Trigger::Webhook(w) => TriggerKey(format!("hook:{}", w.delivery_id)),
            Trigger::Schedule(s) => TriggerKey(format!("slot:{}@{}", s.name, s.slot)),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Trigger::Message(_) => "message",
            Trigger::Webhook(_) => "webhook",
            Trigger::Schedule(_) => "schedule",
        }
    }

    /// A ULID the trigger already carries, used to keep the ids of the
    /// messages a job posts stable across a retry.
    pub(crate) fn ulid_hint(&self) -> Option<ulid::Ulid> {
        let id = match self {
            Trigger::Message(m) => &m.envelope_id,
            Trigger::Webhook(w) => &w.delivery_id,
            // The slot's own time: every host derives the same message ids.
            Trigger::Schedule(s) => return Some(ulid::Ulid::from_parts(s.slot.saturating_mul(1000), 0)),
        };
        ulid::Ulid::from_string(id).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_prefixed_by_kind_so_a_delivery_id_cannot_pose_as_an_envelope() {
        let msg = Trigger::Message(IncomingMessage {
            envelope_id: "X".into(),
            channel_id: "c".into(),
            sender_id: "u".into(),
            text: None,
            reply_to_id: None,
            thread_id: None,
            sent_at: String::new(),
        });
        let hook = Trigger::Webhook(WebhookCall {
            delivery_id: "X".into(),
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        });
        assert_eq!(msg.key().as_str(), "msg:X");
        assert_eq!(hook.key().as_str(), "hook:X");
        assert_ne!(msg.key(), hook.key());
    }
}
