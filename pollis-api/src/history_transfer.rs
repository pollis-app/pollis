//! History transfer between two of one account's enrolled devices (#1735).
//!
//! The DS relays opaque, sealed chunks; it binds every call to the signing
//! device and answers the same 404 to a stranger, a wrong party and an expired
//! transfer. Protocol and threat model: `docs/history-transfer-design.md`.
//!
//! Wire types only; see `pollis-delivery/src/history_transfer.rs`. Every blob is
//! base64 (STANDARD). Times are unix seconds.

use serde::{Deserialize, Serialize};

/// The DS's view of a transfer. `Completed`, `Declined` and `Cancelled` are
/// kept for an hour after they happen, so the other device learns how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    Requested,
    Accepted,
    Completed,
    Declined,
    Cancelled,
}

impl TransferState {
    pub fn as_str(self) -> &'static str {
        match self {
            TransferState::Requested => "requested",
            TransferState::Accepted => "accepted",
            TransferState::Completed => "completed",
            TransferState::Declined => "declined",
            TransferState::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "requested" => TransferState::Requested,
            "accepted" => TransferState::Accepted,
            "completed" => TransferState::Completed,
            "declined" => TransferState::Declined,
            "cancelled" => TransferState::Cancelled,
            _ => return None,
        })
    }

    /// Requested or accepted: still going.
    pub fn is_open(self) -> bool {
        matches!(self, TransferState::Requested | TransferState::Accepted)
    }
}

/// `POST /v1/history-transfer/request` — from the RECEIVING device. The DS
/// takes the receiver from the signature, never from the body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTransferBody {
    /// Client-chosen ULID; also inside the signed `req`.
    pub transfer_id: String,
    /// The sibling asked to send its history.
    pub src_device_id: String,
    /// `link` (right after a device link) or `sibling` (later backfill).
    pub via: String,
    /// The signed request bytes, opaque to the DS.
    pub req: String,
    /// The receiver's ML-DSA-44 signature over the domain-separated `req`.
    pub req_sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestTransferResponse {
    pub expires_at: i64,
}

/// `POST /v1/history-transfer/pending` — the open requests addressed to the
/// signing device as the source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTransfersBody {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransfersResponse {
    pub requests: Vec<PendingTransfer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransfer {
    pub transfer_id: String,
    pub dst_device_id: String,
    pub via: String,
    pub req: String,
    pub req_sig: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// `POST /v1/history-transfer/status` — either party.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferStatusBody {
    pub transfer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferStatusResponse {
    pub transfer_id: String,
    pub src_device_id: String,
    pub dst_device_id: String,
    pub via: String,
    pub state: TransferState,
    pub req: String,
    pub req_sig: String,
    pub acc: Option<String>,
    pub acc_sig: Option<String>,
    /// Highest chunk the receiver acknowledged; -1 = none.
    pub acked_through: i64,
    /// Highest chunk the DS holds right now; -1 = none.
    pub stored_through: i64,
    pub expires_at: i64,
}

/// `POST /v1/history-transfer/accept` — from the SOURCE, after its user approved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptTransferBody {
    pub transfer_id: String,
    pub acc: String,
    pub acc_sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptTransferResponse {
    pub expires_at: i64,
}

/// `POST /v1/history-transfer/decline` — from the SOURCE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclineTransferBody {
    pub transfer_id: String,
}

/// `POST /v1/history-transfer/cancel` — either party.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelTransferBody {
    pub transfer_id: String,
}

/// `POST /v1/history-transfer/complete` — from the RECEIVER, after the final chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteTransferBody {
    pub transfer_id: String,
}

/// `POST /v1/history-transfer/chunk/put` — from the SOURCE. Idempotent for the
/// same bytes; different bytes at a stored index are a 409.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutChunkBody {
    pub transfer_id: String,
    pub idx: i64,
    pub sealed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutChunkResponse {
    pub acked_through: i64,
}

/// `POST /v1/history-transfer/chunk/get` — from the RECEIVER.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetChunkBody {
    pub transfer_id: String,
    pub idx: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetChunkResponse {
    /// `None` until the source has uploaded it.
    pub sealed: Option<String>,
    pub acked_through: i64,
}

/// `POST /v1/history-transfer/chunk/ack` — from the RECEIVER, after the chunk
/// is imported. Deletes it from the DS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckChunkBody {
    pub transfer_id: String,
    pub idx: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckChunkResponse {
    pub acked_through: i64,
}
