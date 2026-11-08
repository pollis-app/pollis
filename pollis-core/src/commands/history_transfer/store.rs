//! `history_transfer_local` / `history_transfer_outgoing` access (#1735).
//! Everything here is synchronous and runs under the caller's local-DB lock.

use rusqlite::{Connection, OptionalExtension};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Src,
    Dst,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::Src => "src",
            Role::Dst => "dst",
        }
    }
}

/// The stored states. `Paused` is an open state like `Transferring`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    AwaitingApproval,
    Transferring,
    Paused,
    Completed,
    Declined,
    Cancelled,
    Expired,
    Failed,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            State::AwaitingApproval => "awaiting_approval",
            State::Transferring => "transferring",
            State::Paused => "paused",
            State::Completed => "completed",
            State::Declined => "declined",
            State::Cancelled => "cancelled",
            State::Expired => "expired",
            State::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "awaiting_approval" => State::AwaitingApproval,
            "transferring" => State::Transferring,
            "paused" => State::Paused,
            "completed" => State::Completed,
            "declined" => State::Declined,
            "cancelled" => State::Cancelled,
            "expired" => State::Expired,
            "failed" => State::Failed,
            _ => return None,
        })
    }

    pub(crate) fn is_ended(self) -> bool {
        !matches!(self, State::AwaitingApproval | State::Transferring | State::Paused)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalRow {
    pub transfer_id: String,
    pub role: Role,
    pub peer_device_id: String,
    pub peer_device_name: Option<String>,
    pub via: String,
    pub state: State,
    pub kem_sk: Option<zeroize::Zeroizing<Vec<u8>>>,
    pub transfer_key: Option<zeroize::Zeroizing<[u8; 32]>>,
    pub req: Vec<u8>,
    pub acc: Option<Vec<u8>>,
    pub plan: Option<String>,
    pub cursor: Option<String>,
    pub next_index: u32,
    pub prev_hash: [u8; 32],
    pub final_index: Option<u32>,
    pub chunks_done: u32,
    pub rows_done: u64,
    pub rows_skipped: u64,
    pub deferred_count: u32,
    pub estimated_total: Option<u64>,
    pub error: Option<String>,
    pub updated_at: String,
}

const COLUMNS: &str = "transfer_id, role, peer_device_id, peer_device_name, via, state, kem_sk, transfer_key, \
     req, acc, plan, cursor, next_index, prev_hash, final_index, chunks_done, rows_done, rows_skipped, \
     deferred_count, error, updated_at, estimated_total";

fn corrupt(what: &str) -> Error {
    Error::Other(anyhow::anyhow!("history transfer row is corrupt: {what}"))
}

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(LocalRowRaw,)> {
    Ok((LocalRowRaw {
        transfer_id: r.get(0)?,
        role: r.get(1)?,
        peer_device_id: r.get(2)?,
        peer_device_name: r.get(3)?,
        via: r.get(4)?,
        state: r.get(5)?,
        kem_sk: r.get(6)?,
        transfer_key: r.get(7)?,
        req: r.get(8)?,
        acc: r.get(9)?,
        plan: r.get(10)?,
        cursor: r.get(11)?,
        next_index: r.get(12)?,
        prev_hash: r.get(13)?,
        final_index: r.get(14)?,
        chunks_done: r.get(15)?,
        rows_done: r.get(16)?,
        rows_skipped: r.get(17)?,
        deferred_count: r.get(18)?,
        error: r.get(19)?,
        updated_at: r.get(20)?,
        estimated_total: r.get(21)?,
    },))
}

struct LocalRowRaw {
    transfer_id: String,
    role: String,
    peer_device_id: String,
    peer_device_name: Option<String>,
    via: String,
    state: String,
    kem_sk: Option<Vec<u8>>,
    transfer_key: Option<Vec<u8>>,
    req: Vec<u8>,
    acc: Option<Vec<u8>>,
    plan: Option<String>,
    cursor: Option<String>,
    next_index: i64,
    prev_hash: Vec<u8>,
    final_index: Option<i64>,
    chunks_done: i64,
    rows_done: i64,
    rows_skipped: i64,
    deferred_count: i64,
    error: Option<String>,
    updated_at: String,
    estimated_total: Option<i64>,
}

impl LocalRowRaw {
    fn into_row(self) -> Result<LocalRow> {
        let role = match self.role.as_str() {
            "src" => Role::Src,
            "dst" => Role::Dst,
            _ => return Err(corrupt("role")),
        };
        let state = State::parse(&self.state).ok_or_else(|| corrupt("state"))?;
        let transfer_key = match self.transfer_key {
            Some(k) => Some(zeroize::Zeroizing::new(
                <[u8; 32]>::try_from(k.as_slice()).map_err(|_| corrupt("key"))?,
            )),
            None => None,
        };
        Ok(LocalRow {
            transfer_id: self.transfer_id,
            role,
            peer_device_id: self.peer_device_id,
            peer_device_name: self.peer_device_name,
            via: self.via,
            state,
            kem_sk: self.kem_sk.map(zeroize::Zeroizing::new),
            transfer_key,
            req: self.req,
            acc: self.acc,
            plan: self.plan,
            cursor: self.cursor,
            next_index: u32::try_from(self.next_index).map_err(|_| corrupt("next_index"))?,
            prev_hash: <[u8; 32]>::try_from(self.prev_hash.as_slice()).map_err(|_| corrupt("prev_hash"))?,
            final_index: self.final_index.map(|i| u32::try_from(i).map_err(|_| corrupt("final_index"))).transpose()?,
            chunks_done: u32::try_from(self.chunks_done).unwrap_or(u32::MAX),
            rows_done: u64::try_from(self.rows_done).unwrap_or(0),
            rows_skipped: u64::try_from(self.rows_skipped).unwrap_or(0),
            deferred_count: u32::try_from(self.deferred_count).unwrap_or(0),
            estimated_total: self.estimated_total.and_then(|n| u64::try_from(n).ok()),
            error: self.error,
            updated_at: self.updated_at,
        })
    }
}

pub(crate) fn load(conn: &Connection, transfer_id: &str) -> Result<Option<LocalRow>> {
    let raw = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM history_transfer_local WHERE transfer_id = ?1"),
            rusqlite::params![transfer_id],
            from_row,
        )
        .optional()?;
    raw.map(|(r,)| r.into_row()).transpose()
}

/// The most recent transfers, newest first.
pub(crate) fn list(conn: &Connection, limit: i64) -> Result<Vec<LocalRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM history_transfer_local ORDER BY updated_at DESC, transfer_id DESC LIMIT ?1"
    ))?;
    let raws: Vec<(LocalRowRaw,)> = stmt
        .query_map(rusqlite::params![limit], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    raws.into_iter().map(|(r,)| r.into_row()).collect()
}

/// The open (not ended) transfers.
pub(crate) fn open_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT transfer_id FROM history_transfer_local
         WHERE state IN ('awaiting_approval', 'transferring', 'paused') ORDER BY created_at",
    )?;
    let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The receiver's open transfer, if any (at most one, by index).
pub(crate) fn open_receiving(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT transfer_id FROM history_transfer_local
             WHERE role = 'dst' AND state IN ('awaiting_approval', 'transferring', 'paused')",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// A strictly later stamp than `old`, in the column's lexically ordered format.
pub(crate) fn next_stamp(old: &str) -> String {
    let now = chrono::Utc::now();
    let floor = chrono::DateTime::parse_from_rfc3339(old)
        .map(|t| t.with_timezone(&chrono::Utc) + chrono::Duration::milliseconds(1))
        .unwrap_or(now);
    now.max(floor).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Bump `updated_at` (strictly forward) — every progress write goes through this.
pub(crate) fn touch(conn: &Connection, transfer_id: &str) -> Result<()> {
    let old: String = conn.query_row(
        "SELECT updated_at FROM history_transfer_local WHERE transfer_id = ?1",
        rusqlite::params![transfer_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE history_transfer_local SET updated_at = ?2 WHERE transfer_id = ?1",
        rusqlite::params![transfer_id, next_stamp(&old)],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn insert(
    conn: &Connection,
    transfer_id: &str,
    role: Role,
    peer_device_id: &str,
    peer_device_name: Option<&str>,
    via: &str,
    state: State,
    kem_sk: Option<&[u8]>,
    transfer_key: Option<&[u8; 32]>,
    req: &[u8],
    acc: Option<&[u8]>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO history_transfer_local
         (transfer_id, role, peer_device_id, peer_device_name, via, state, kem_sk, transfer_key, req, acc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            transfer_id,
            role.as_str(),
            peer_device_id,
            peer_device_name,
            via,
            state.as_str(),
            kem_sk,
            transfer_key.map(|k| k.to_vec()),
            req,
            acc
        ],
    )?;
    Ok(())
}

/// Move to an open state (`transferring` / `paused`), recording `error` (None clears it).
pub(crate) fn set_open_state(conn: &Connection, transfer_id: &str, state: State, error: Option<&str>) -> Result<()> {
    debug_assert!(!state.is_ended());
    conn.execute(
        "UPDATE history_transfer_local SET state = ?2, error = ?3 WHERE transfer_id = ?1",
        rusqlite::params![transfer_id, state.as_str(), error],
    )?;
    touch(conn, transfer_id)
}

/// End a transfer: the state, the reason, and every secret and stored chunk gone.
pub(crate) fn end(conn: &Connection, transfer_id: &str, state: State, error: Option<&str>) -> Result<()> {
    debug_assert!(state.is_ended());
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE history_transfer_local
         SET state = ?2, error = COALESCE(?3, error), kem_sk = NULL, transfer_key = NULL
         WHERE transfer_id = ?1 AND state IN ('awaiting_approval', 'transferring', 'paused')",
        rusqlite::params![transfer_id, state.as_str(), error],
    )?;
    tx.execute(
        "DELETE FROM history_transfer_outgoing WHERE transfer_id = ?1",
        rusqlite::params![transfer_id],
    )?;
    touch(&tx, transfer_id)?;
    tx.commit()?;
    Ok(())
}

/// Ended transfers older than this are forgotten (their ids stay refused by
/// the request's freshness bound long before).
pub(crate) fn prune(conn: &Connection) {
    let _ = conn.execute(
        "DELETE FROM history_transfer_local
         WHERE state IN ('completed', 'declined', 'cancelled', 'expired', 'failed')
           AND updated_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days')",
        [],
    );
}

pub(crate) fn outgoing(conn: &Connection, transfer_id: &str) -> Result<Vec<(u32, Vec<u8>)>> {
    let mut stmt = conn.prepare(
        "SELECT idx, sealed FROM history_transfer_outgoing WHERE transfer_id = ?1 ORDER BY idx",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![transfer_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(i, b)| Ok((u32::try_from(i).map_err(|_| corrupt("outgoing idx"))?, b)))
        .collect()
}

/// Drop stored chunks the receiver acknowledged and record the progress.
pub(crate) fn note_acked(conn: &Connection, transfer_id: &str, acked_through: i64) -> Result<bool> {
    if acked_through < 0 {
        return Ok(false);
    }
    let removed = conn.execute(
        "DELETE FROM history_transfer_outgoing WHERE transfer_id = ?1 AND idx <= ?2",
        rusqlite::params![transfer_id, acked_through],
    )?;
    let moved = conn.execute(
        "UPDATE history_transfer_local SET chunks_done = ?2 WHERE transfer_id = ?1 AND chunks_done < ?2",
        rusqlite::params![transfer_id, acked_through + 1],
    )?;
    if removed > 0 || moved > 0 {
        touch(conn, transfer_id)?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn the_local_schema_refuses_invalid_transfer_states() {
        let conn = db();
        insert(&conn, "A", Role::Dst, "peer", None, "sibling", State::AwaitingApproval, Some(&[1u8; 96]), None, b"req", None).unwrap();
        // One open receiving transfer at a time.
        assert!(insert(&conn, "B", Role::Dst, "peer", None, "sibling", State::Transferring, None, Some(&[2u8; 32]), b"req", None).is_err());
        // A source never holds a receiver secret.
        assert!(insert(&conn, "C", Role::Src, "peer", None, "sibling", State::Transferring, Some(&[1u8; 96]), None, b"req", None).is_err());
        // An ended transfer cannot hold a key.
        assert!(insert(&conn, "D", Role::Src, "peer", None, "sibling", State::Completed, None, Some(&[2u8; 32]), b"req", None).is_err());
        // A key is 32 bytes.
        assert!(conn
            .execute("UPDATE history_transfer_local SET transfer_key = X'0102' WHERE transfer_id = 'A'", [])
            .is_err());
        // next_index only moves forward.
        conn.execute("UPDATE history_transfer_local SET next_index = 5 WHERE transfer_id = 'A'", []).unwrap();
        assert!(conn.execute("UPDATE history_transfer_local SET next_index = 4 WHERE transfer_id = 'A'", []).is_err());
        // Ended stays ended, and ending zeroizes.
        end(&conn, "A", State::Cancelled, Some("x")).unwrap();
        let row = load(&conn, "A").unwrap().unwrap();
        assert!(row.kem_sk.is_none() && row.transfer_key.is_none());
        assert!(conn
            .execute("UPDATE history_transfer_local SET state = 'transferring' WHERE transfer_id = 'A'", [])
            .is_err());
        // With A ended, a new receiving transfer is allowed.
        insert(&conn, "B", Role::Dst, "peer", None, "sibling", State::AwaitingApproval, Some(&[1u8; 96]), None, b"req", None).unwrap();
    }

    #[test]
    fn stamps_only_move_forward() {
        let far = "2999-01-01T00:00:00.000Z";
        let next = next_stamp(far);
        assert!(next.as_str() > far, "{next}");
        let now_ish = next_stamp("2000-01-01T00:00:00.000Z");
        assert!(now_ish.as_str() > "2026-01-01");
    }
}
