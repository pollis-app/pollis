//! Thin wrappers over production code for the detached `fuzz/` crate (#1537).

pub use super::framing::{
    classify, pad, pad_receipt, pad_redaction, pad_threaded, strip, thread_of, Frame, ReceiptKind,
};
pub use rusqlite;

/// [`super::ingest`]'s per-epoch decrypt order: the run of `(conversation,
/// envelope)` indices sorted by the DS's group-wide arrival position.
pub fn into_sealing_order(run: &mut [(usize, usize)], arrival: &[Vec<Option<i64>>]) {
    super::ingest::into_sealing_order(run, arrival)
}

/// One catch-up's envelopes, bucketed per listed conversation the way ingest
/// buckets them: `(conversation, [(envelope id, seq)])` in listing order, plus
/// each envelope's arrival position in the same shape.
pub type Partitioned = (Vec<(String, Vec<(String, i64)>)>, Vec<Vec<Option<i64>>>);

/// [`super::ingest`]'s partition of a catch-up envelope list.
pub fn partition_envelopes(
    conversation_ids: Vec<String>,
    envelopes: Vec<pollis_api::directory::EnvelopeWire>,
) -> Partitioned {
    let (per_conv, arrival) = super::ingest::partition_envelopes(conversation_ids, envelopes);
    let per_conv = per_conv
        .into_iter()
        .map(|(cid, envs)| (cid, envs.into_iter().map(|e| (e.0, e.7)).collect()))
        .collect();
    (per_conv, arrival)
}

/// The outbox's row transitions on a fresh local schema, one connection.
pub struct Outbox {
    conn: rusqlite::Connection,
}

impl Outbox {
    pub fn new() -> crate::error::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        crate::db::local::apply_local_schema(&conn)?;
        Ok(Self { conn })
    }

    /// A send's first step: store the row, or adopt the one already there.
    /// `Ok(true)` when the id was already delivered.
    pub fn enqueue(
        &self,
        id: &str,
        conversation_id: &str,
        sender_id: &str,
        content: &str,
        sent_at: &str,
    ) -> crate::error::Result<bool> {
        let queued = super::outbox::Queued {
            id: id.to_string(),
            conversation_id: conversation_id.to_string(),
            sender_id: sender_id.to_string(),
            content: content.to_string(),
            reply_to_id: None,
            thread_id: None,
            sent_at: sent_at.to_string(),
        };
        Ok(matches!(
            super::outbox::enqueue_on(&self.conn, &queued, &[])?,
            super::outbox::Enqueued::AlreadySent(_)
        ))
    }

    /// A failed attempt: `rejected` when no retry can fix it.
    pub fn fail(&self, id: &str, permanent: bool) -> crate::error::Result<()> {
        let state = if permanent {
            super::outbox::REJECTED
        } else {
            super::outbox::FAILED
        };
        super::outbox::mark_state_on(&self.conn, id, state)
    }

    /// The DS took it.
    pub fn settle(&self, id: &str) -> crate::error::Result<String> {
        super::outbox::settle_on(&self.conn, id)
    }

    /// Discard an unsent message. `Ok(false)` when there was none to discard.
    pub fn discard(&self, id: &str) -> crate::error::Result<bool> {
        super::outbox::remove_unsent_on(&self.conn, id)
    }

    /// What the drain would try, in order.
    pub fn unsent(&self) -> crate::error::Result<Vec<String>> {
        super::outbox::unsent_ids_on(&self.conn)
    }

    /// `None` = no row; `Some(None)` = delivered; `Some(Some(state))` = unsent.
    pub fn state(&self, id: &str) -> crate::error::Result<Option<Option<String>>> {
        Ok(super::outbox::load(&self.conn, id)?.map(|(_, s)| s))
    }

    /// The connection, for a target's deliberately broken mutant.
    pub fn conn(&self) -> &rusqlite::Connection {
        &self.conn
    }

    /// How many `message` rows carry this id (the table's key makes it ≤ 1).
    pub fn rows(&self, id: &str) -> crate::error::Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM message WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )?)
    }
}
