//! The bytes two devices exchange in a history transfer (#1735), and the
//! receiver's validation of them. Pure: the fuzz target drives [`parse_chunk`]
//! and [`check_row`] directly.
//!
//! The rule for a chunk the receiver cannot accept (A6):
//! - **Abort the transfer** on anything that says the stream itself is wrong —
//!   AEAD failure (in `crypto::open`), an unknown version, a framing or bounds
//!   error, a wrong index or transfer id, a broken `prev_hash` chain, a final
//!   flag that disagrees with the body. Nothing from that chunk is written.
//! - **Skip and count** a single ROW that breaks a row rule (not a ULID,
//!   content and a delete both set, the `sealed` placeholder author, an
//!   unparsable date, oversize fields). One odd legacy row on the source must
//!   never wedge a transfer forever.

use serde::{Deserialize, Serialize};

/// Transcript and chunk format version.
pub const VERSION: u8 = 1;

/// A chunk's plaintext stays at or under this (the sealed cap on the DS is 1 MiB).
pub const PLAIN_TARGET_BYTES: usize = 512 * 1024;
/// Hard bound on a plaintext the receiver will parse.
pub const PLAIN_MAX_BYTES: usize = 1024 * 1024 - 64;
/// Rows per chunk, at most.
pub const MAX_ROWS: usize = 4000;
/// Receipts per chunk, at most.
pub const MAX_RECEIPTS: usize = 16_000;
/// Conversations a request may list, and deferred ids a final chunk may carry.
pub const MAX_CONVERSATIONS: usize = 8192;
/// One message body, at most (attachments travel as `_att` references).
pub const MAX_CONTENT_BYTES: usize = 256 * 1024;
/// Every id and timestamp field, at most.
pub const MAX_FIELD_BYTES: usize = 128;
/// The placeholder author (#1312); never a real row's sender.
pub const SEALED_SENDER: &str = "sealed";

/// One conversation the receiver holds an MLS group for, with where it joined
/// that group's current lineage (A2: the source must be at least there).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReqConversation {
    pub conversation_id: String,
    pub mls_group_id: String,
    pub join_generation: i64,
    pub join_epoch: u64,
    pub is_dm: bool,
}

/// The receiver's signed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Req {
    pub v: u8,
    pub transfer_id: String,
    pub user_id: String,
    pub dst_device_id: String,
    pub src_device_id: String,
    /// base64
    pub kem_pub_x25519: String,
    /// base64
    pub kem_pub_mlkem: String,
    /// The receiver's retention window in days (0 = forever).
    pub retention_days: i64,
    pub via: String,
    /// Unix seconds, the receiver's clock.
    pub created_at: i64,
    pub conversations: Vec<ReqConversation>,
}

/// The source's signed acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acc {
    pub v: u8,
    pub transfer_id: String,
    pub src_device_id: String,
    pub dst_device_id: String,
    /// hex SHA-256 of the exact request bytes.
    pub req_hash: String,
    /// base64
    pub kem_ct_x25519: String,
    /// base64
    pub kem_ct_mlkem: String,
}

/// One message row, exactly as the source stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRow {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    /// NULL only for a deleted message.
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    /// Byte-identical to the source (it is the ordering key).
    pub sent_at: String,
    pub received_at: String,
    pub edited_at: Option<String>,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireReceipt {
    pub message_id: String,
    pub reader_id: String,
    pub kind: String,
    pub at: String,
}

/// Carried by the final chunk only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalInfo {
    /// Chunks in the whole transfer, this one included.
    pub total_chunks: u32,
    /// Rows in the whole transfer (every chunk's `rows.len()` summed).
    pub total_rows: u64,
    /// Rows the source left out as unsendable (oversize).
    pub source_skipped: u64,
    /// Conversations the source was not ready to send (A2); copy again later.
    pub deferred: Vec<String>,
}

/// One chunk's plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkPlain {
    pub v: u8,
    pub transfer_id: String,
    pub index: u32,
    /// hex SHA-256 of the previous chunk's plaintext; 64 zeros for chunk 0.
    pub prev_hash: String,
    pub rows: Vec<WireRow>,
    pub receipts: Vec<WireReceipt>,
    #[serde(rename = "final")]
    pub fin: Option<FinalInfo>,
    /// Chunk 0 only: the rows the source expects to send (a progress hint).
    #[serde(default)]
    pub estimated_total: Option<u64>,
}

/// Why a chunk ends the transfer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("chunk too large")]
    TooLarge,
    #[error("chunk does not parse")]
    Parse,
    #[error("unknown chunk version")]
    Version,
    #[error("chunk names another transfer")]
    Transfer,
    #[error("chunk out of order")]
    Index,
    #[error("chunk does not follow the previous one")]
    Chain,
    #[error("too many rows or receipts")]
    Bounds,
    #[error("final flag and final totals disagree")]
    Final,
}

pub const ZERO_HASH: [u8; 32] = [0u8; 32];

/// Parse and check one opened chunk against where the receiver stands.
pub fn parse_chunk(
    plain: &[u8],
    transfer_id: &str,
    index: u32,
    prev_hash: &[u8; 32],
    is_final: bool,
) -> Result<ChunkPlain, FrameError> {
    if plain.len() > PLAIN_MAX_BYTES {
        return Err(FrameError::TooLarge);
    }
    let chunk: ChunkPlain = serde_json::from_slice(plain).map_err(|_| FrameError::Parse)?;
    if chunk.v != VERSION {
        return Err(FrameError::Version);
    }
    if chunk.transfer_id != transfer_id {
        return Err(FrameError::Transfer);
    }
    if chunk.index != index {
        return Err(FrameError::Index);
    }
    if chunk.prev_hash != hex::encode(prev_hash) {
        return Err(FrameError::Chain);
    }
    if chunk.rows.len() > MAX_ROWS || chunk.receipts.len() > MAX_RECEIPTS {
        return Err(FrameError::Bounds);
    }
    match (&chunk.fin, is_final) {
        (Some(f), true) => {
            if f.total_chunks != index.saturating_add(1) || f.deferred.len() > MAX_CONVERSATIONS {
                return Err(FrameError::Final);
            }
        }
        (None, false) => {}
        _ => return Err(FrameError::Final),
    }
    Ok(chunk)
}

/// Why a single row is skipped (counted, never fatal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSkip {
    Id,
    Field,
    Sender,
    Tombstone,
    Date,
    Oversize,
}

/// A row that passed [`check_row`], with `received_at` normalized to
/// `YYYY-MM-DD HH:MM:SS` UTC (the format eviction compares as text) and never
/// later than `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedRow {
    pub row: WireRow,
    pub received_at: String,
}

fn parse_instant(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&chrono::Utc));
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(n.and_utc());
        }
    }
    None
}

fn field_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_FIELD_BYTES
}

/// Check one row against the row rules (A6).
pub fn check_row(row: &WireRow, now: chrono::DateTime<chrono::Utc>) -> Result<CheckedRow, RowSkip> {
    if ulid::Ulid::from_string(&row.id).is_err() {
        return Err(RowSkip::Id);
    }
    if !field_ok(&row.conversation_id)
        || !field_ok(&row.sent_at)
        || !field_ok(&row.received_at)
        || row.reply_to_id.as_deref().is_some_and(|s| !field_ok(s))
        || row.thread_id.as_deref().is_some_and(|s| !field_ok(s))
        || row.edited_at.as_deref().is_some_and(|s| !field_ok(s))
        || row.deleted_at.as_deref().is_some_and(|s| !field_ok(s))
    {
        return Err(RowSkip::Field);
    }
    if !field_ok(&row.sender_id) || row.sender_id == SEALED_SENDER {
        return Err(RowSkip::Sender);
    }
    // A live row has content and no delete; a tombstone has a delete and no content.
    if row.content.is_some() == row.deleted_at.is_some() {
        return Err(RowSkip::Tombstone);
    }
    if row.content.as_deref().is_some_and(|c| c.len() > MAX_CONTENT_BYTES) {
        return Err(RowSkip::Oversize);
    }
    if parse_instant(&row.sent_at).is_none()
        || row.edited_at.as_deref().is_some_and(|s| parse_instant(s).is_none())
        || row.deleted_at.as_deref().is_some_and(|s| parse_instant(s).is_none())
    {
        return Err(RowSkip::Date);
    }
    let Some(received) = parse_instant(&row.received_at) else {
        return Err(RowSkip::Date);
    };
    let received = received.min(now);
    Ok(CheckedRow { row: row.clone(), received_at: received.format("%Y-%m-%d %H:%M:%S").to_string() })
}

/// A receipt the receiver may store: plausible fields, a known kind.
pub fn receipt_ok(r: &WireReceipt) -> bool {
    field_ok(&r.message_id)
        && field_ok(&r.reader_id)
        && field_ok(&r.at)
        && (r.kind == "delivered" || r.kind == "read")
}

/// Bounds on a request the source will consider.
pub fn req_ok(req: &Req) -> bool {
    req.v == VERSION
        && field_ok(&req.transfer_id)
        && field_ok(&req.user_id)
        && field_ok(&req.dst_device_id)
        && field_ok(&req.src_device_id)
        && (req.via == "link" || req.via == "sibling")
        && crate::db::local::ALLOWED_RETENTION_DAYS.contains(&req.retention_days)
        && req.conversations.len() <= MAX_CONVERSATIONS
        && req
            .conversations
            .iter()
            .all(|c| field_ok(&c.conversation_id) && field_ok(&c.mls_group_id) && c.join_generation >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> WireRow {
        WireRow {
            id: ulid::Ulid::new().to_string(),
            conversation_id: "c".into(),
            sender_id: "bob".into(),
            content: Some("hi".into()),
            reply_to_id: None,
            thread_id: None,
            sent_at: "2026-01-01T00:00:00.123Z".into(),
            received_at: "2026-01-01 00:00:01".into(),
            edited_at: None,
            deleted_at: None,
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    #[test]
    fn row_rules_skip_bad_rows() {
        assert!(check_row(&row(), now()).is_ok());
        let mut r = row();
        r.id = "not-a-ulid".into();
        assert_eq!(check_row(&r, now()), Err(RowSkip::Id));
        let mut r = row();
        r.sender_id = SEALED_SENDER.into();
        assert_eq!(check_row(&r, now()), Err(RowSkip::Sender));
        let mut r = row();
        r.deleted_at = Some("2026-01-02T00:00:00Z".into());
        assert_eq!(check_row(&r, now()), Err(RowSkip::Tombstone), "content AND a delete");
        let mut r = row();
        r.content = None;
        assert_eq!(check_row(&r, now()), Err(RowSkip::Tombstone), "neither content nor a delete");
        let mut r = row();
        r.sent_at = "yesterday".into();
        assert_eq!(check_row(&r, now()), Err(RowSkip::Date));
        let mut r = row();
        r.content = Some("x".repeat(MAX_CONTENT_BYTES + 1));
        assert_eq!(check_row(&r, now()), Err(RowSkip::Oversize));
        let mut r = row();
        r.conversation_id = "c".repeat(MAX_FIELD_BYTES + 1);
        assert_eq!(check_row(&r, now()), Err(RowSkip::Field));
        // A tombstone row is fine.
        let mut r = row();
        r.content = None;
        r.deleted_at = Some("2026-01-02T00:00:00+00:00".into());
        assert!(check_row(&r, now()).is_ok());
    }

    #[test]
    fn received_at_is_normalized_and_never_in_the_future() {
        let mut r = row();
        r.received_at = "2026-01-01T05:06:07.890+02:00".into();
        assert_eq!(check_row(&r, now()).unwrap().received_at, "2026-01-01 03:06:07");
        let mut r = row();
        r.received_at = "2999-01-01 00:00:00".into();
        let n = now();
        assert_eq!(check_row(&r, n).unwrap().received_at, n.format("%Y-%m-%d %H:%M:%S").to_string());
        // sent_at is carried byte-for-byte.
        let r = row();
        assert_eq!(check_row(&r, now()).unwrap().row.sent_at, r.sent_at);
    }

    fn chunk(index: u32, prev: &[u8; 32], fin: Option<FinalInfo>) -> Vec<u8> {
        serde_json::to_vec(&ChunkPlain {
            v: VERSION,
            transfer_id: "T".into(),
            index,
            prev_hash: hex::encode(prev),
            rows: vec![row()],
            receipts: vec![],
            fin,
            estimated_total: None,
        })
        .unwrap()
    }

    #[test]
    fn a_chunk_must_continue_the_chain_at_the_expected_index() {
        let c0 = chunk(0, &ZERO_HASH, None);
        assert!(parse_chunk(&c0, "T", 0, &ZERO_HASH, false).is_ok());
        assert_eq!(parse_chunk(&c0, "U", 0, &ZERO_HASH, false), Err(FrameError::Transfer));
        assert_eq!(parse_chunk(&c0, "T", 1, &ZERO_HASH, false), Err(FrameError::Index));
        assert_eq!(parse_chunk(&c0, "T", 0, &[1u8; 32], false), Err(FrameError::Chain));
        assert_eq!(parse_chunk(&c0, "T", 0, &ZERO_HASH, true), Err(FrameError::Final));
        let h0 = crate::commands::history_transfer::crypto::sha256(&c0);
        let fin = FinalInfo { total_chunks: 2, total_rows: 2, source_skipped: 0, deferred: vec![] };
        let c1 = chunk(1, &h0, Some(fin.clone()));
        assert!(parse_chunk(&c1, "T", 1, &h0, true).is_ok());
        assert_eq!(parse_chunk(&c1, "T", 1, &h0, false), Err(FrameError::Final));
        let wrong = chunk(1, &h0, Some(FinalInfo { total_chunks: 5, ..fin }));
        assert_eq!(parse_chunk(&wrong, "T", 1, &h0, true), Err(FrameError::Final));
        assert_eq!(parse_chunk(b"{not json", "T", 0, &ZERO_HASH, false), Err(FrameError::Parse));
        let mut v2: serde_json::Value = serde_json::from_slice(&c0).unwrap();
        v2["v"] = 2.into();
        assert_eq!(parse_chunk(&serde_json::to_vec(&v2).unwrap(), "T", 0, &ZERO_HASH, false), Err(FrameError::Version));
        // An unknown field anywhere is a framing error, not silently ignored.
        let mut extra: serde_json::Value = serde_json::from_slice(&c0).unwrap();
        extra["rows"][0]["ciphertext"] = "x".into();
        assert_eq!(parse_chunk(&serde_json::to_vec(&extra).unwrap(), "T", 0, &ZERO_HASH, false), Err(FrameError::Parse));
    }
}
