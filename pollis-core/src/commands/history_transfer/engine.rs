//! The resumable loops behind `run_history_transfer` (#1735).
//!
//! Both roles are driven from the local row alone, so a run can stop at any
//! point (crash, lock, idle) and the next run continues exactly where the
//! committed state says:
//!
//! - **Source.** A chunk is sealed and stored (`history_transfer_outgoing`)
//!   in the SAME transaction that advances `next_index`, cursor and chain hash,
//!   and only then uploaded. A restart re-uploads the stored bytes verbatim
//!   (the DS accepts identical bytes, refuses different ones); an index is
//!   never sealed twice. Stored chunks go once the receiver acknowledges them.
//! - **Receiver.** A chunk is opened and fully checked, then imported in ONE
//!   transaction (under the cross-process state lock) that also advances
//!   `next_index` and the chain hash — so a chunk is applied entirely or not at
//!   all. Only then is it acknowledged; an import whose ack was lost is re-acked
//!   on the next run, never re-imported.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::store::{self, LocalRow, Role, State};
use super::{crypto, ds, err, identity, wire, with_db, Ds, HistoryTransfer};
use crate::commands::messages::test_hooks::{self as hooks, Crash};
use crate::error::{Error, Result};
use crate::sink::EventSink;
use crate::state::AppState;

/// The DS's window, mirrored: the source seals at most this far past the ack.
const WINDOW: i64 = 8;

/// Rows the source reads per query (a chunk may hold fewer, by size).
const ROWS_PER_QUERY: i64 = 2000;

/// Receipts' share of a chunk, at most.
const RECEIPT_BYTES: usize = 256 * 1024;

/// Woken by the DS's `history_transfer` nudge (and by local cancels).
static WAKE: Notify = Notify::const_new();
/// Notified after every progress write; `await_history_transfer` and
/// followers of an in-flight run wait on it.
static PROGRESS: Notify = Notify::const_new();
/// `(device, transfer)` pairs with a run in flight in this process.
static RUNNING: std::sync::Mutex<Option<HashSet<(String, String)>>> = std::sync::Mutex::new(None);

#[cfg(feature = "test-harness")]
static IDLE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "test-harness")]
static AUTO_RESUME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test-harness controls for the transfer loops.
#[cfg(feature = "test-harness")]
pub mod transfer_hooks {
    /// Override how long a run waits without progress before it pauses (0 = default).
    pub fn set_idle_limit(limit: std::time::Duration) {
        super::IDLE_MS.store(limit.as_millis() as u64, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether unlock resumes open transfers in the background (off by default in tests).
    pub fn set_auto_resume(on: bool) {
        super::AUTO_RESUME.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn auto_resume_enabled() -> bool {
    #[cfg(feature = "test-harness")]
    {
        AUTO_RESUME.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(feature = "test-harness"))]
    {
        true
    }
}

fn idle_limit(awaiting: bool) -> Duration {
    #[cfg(feature = "test-harness")]
    {
        let ms = IDLE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if ms > 0 {
            return Duration::from_millis(ms);
        }
    }
    if awaiting {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(60)
    }
}

/// A DS-originated `history_transfer` nudge arrived: re-check now.
pub fn wake() {
    WAKE.notify_waiters();
}

pub(crate) fn notify_progress() {
    PROGRESS.notify_waiters();
}

/// Bounded, event-assisted waiting: back off from 200 ms to 2 s, wake early on
/// a nudge, and report idleness after the idle limit without progress.
struct Waiter {
    delay: Duration,
    since: Instant,
}

impl Waiter {
    fn new() -> Self {
        Waiter { delay: Duration::from_millis(200), since: Instant::now() }
    }

    fn progressed(&mut self) {
        self.delay = Duration::from_millis(200);
        self.since = Instant::now();
    }

    /// Wait a step; `true` when the run has been idle past the limit.
    async fn wait(&mut self, awaiting: bool) -> bool {
        if self.since.elapsed() >= idle_limit(awaiting) {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(self.delay) => {}
            _ = WAKE.notified() => {}
        }
        self.delay = (self.delay * 2).min(Duration::from_secs(2));
        false
    }
}

struct RunClaim((String, String));

impl Drop for RunClaim {
    fn drop(&mut self) {
        if let Some(set) = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            set.remove(&self.0);
        }
        notify_progress();
    }
}

fn claim(key: (String, String)) -> Option<RunClaim> {
    let mut guard = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if set.insert(key.clone()) {
        Some(RunClaim(key))
    } else {
        None
    }
}

fn is_running(key: &(String, String)) -> bool {
    RUNNING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|s| s.contains(key))
}

async fn emit(state: &Arc<AppState>, id: &str, sink: &Arc<dyn EventSink<HistoryTransfer>>) -> Result<HistoryTransfer> {
    let snap = super::snapshot(state, id).await?;
    let _ = sink.send(snap.clone());
    notify_progress();
    Ok(snap)
}

/// Drive a transfer, in whichever role this device has, until it ends or
/// pauses (no progress for a bounded while). Emits a snapshot on every step.
/// A second call while one is in flight follows the first instead of racing it.
pub async fn run_history_transfer(
    state: &Arc<AppState>,
    transfer_id: String,
    sink: Arc<dyn EventSink<HistoryTransfer>>,
) -> Result<HistoryTransfer> {
    let (user, device) = identity(state).await?;
    let key = (device.clone(), transfer_id.clone());
    let Some(_claim) = claim(key.clone()) else {
        return follow(state, &transfer_id, &key, &sink).await;
    };
    let row = with_db(state, |c| store::load(c, &transfer_id))
        .await?
        .ok_or_else(|| err("No such transfer."))?;
    if row.state.is_ended() {
        return emit(state, &transfer_id, &sink).await;
    }
    match row.role {
        Role::Src => run_source(state, &user, &device, &transfer_id, &sink).await,
        Role::Dst => run_receiver(state, &user, &device, &transfer_id, &sink).await,
    }
}

async fn follow(
    state: &Arc<AppState>,
    id: &str,
    key: &(String, String),
    sink: &Arc<dyn EventSink<HistoryTransfer>>,
) -> Result<HistoryTransfer> {
    loop {
        let notified = PROGRESS.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let snap = super::snapshot(state, id).await?;
        let _ = sink.send(snap.clone());
        if snap.is_ended() || !is_running(key) {
            return Ok(snap);
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), notified).await;
    }
}

/// The transfer's state as soon as it differs from `since` (its `updated_at`),
/// or after a bounded wait. The mobile host's progress stream: it calls this in
/// a loop next to `run_history_transfer` (uniffi has no event channel).
pub async fn await_history_transfer(
    state: &Arc<AppState>,
    transfer_id: String,
    since: Option<String>,
) -> Result<HistoryTransfer> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let notified = PROGRESS.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let snap = super::snapshot(state, &transfer_id).await?;
        if since.as_deref() != Some(snap.updated_at.as_str()) || Instant::now() >= deadline {
            return Ok(snap);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let _ = tokio::time::timeout(left, notified).await;
    }
}

async fn end(state: &Arc<AppState>, id: &str, to: State, why: Option<&str>) -> Result<()> {
    with_db(state, |c| store::end(c, id, to, why)).await?;
    notify_progress();
    Ok(())
}

/// A protocol violation: end the transfer as failed and tell the DS to drop it.
async fn fail(
    state: &Arc<AppState>,
    id: &str,
    why: &str,
    sink: &Arc<dyn EventSink<HistoryTransfer>>,
) -> Result<HistoryTransfer> {
    crate::diag!("[history-transfer] {id} failed: {why}");
    end(state, id, State::Failed, Some(why)).await?;
    let _ = ds(state, &pollis_api::history_transfer::CancelTransferBody { transfer_id: id.to_string() }).await;
    emit(state, id, sink).await
}

async fn pause(state: &Arc<AppState>, id: &str, sink: &Arc<dyn EventSink<HistoryTransfer>>) -> Result<HistoryTransfer> {
    with_db(state, |c| store::set_open_state(c, id, State::Paused, None)).await?;
    emit(state, id, sink).await
}

async fn load(state: &Arc<AppState>, id: &str) -> Result<LocalRow> {
    with_db(state, |c| store::load(c, id)).await?.ok_or_else(|| err("No such transfer."))
}

fn parse_req(row: &LocalRow) -> Result<wire::Req> {
    serde_json::from_slice(&row.req).map_err(|_| err("stored request is corrupt"))
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// Which of the receiver's conversations this device sends, fixed once (A2).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct Plan {
    eligible: Vec<String>,
    deferred: Vec<String>,
}

/// Decide the plan: a conversation is sent only if this device is a member,
/// maps it to the same MLS group, caught that group up successfully during
/// this transfer, and holds it at or past the receiver's join (generation,
/// epoch). A member conversation that fails any of that is DEFERRED, not sent:
/// this device may not yet reflect an edit or delete the receiver can never see.
async fn make_plan(state: &Arc<AppState>, user: &str, req: &wire::Req) -> Result<Plan> {
    let members = super::membership(state, user).await?;
    let mut by_group: HashMap<String, Vec<&wire::ReqConversation>> = HashMap::new();
    let mut plan = Plan::default();
    for c in &req.conversations {
        match members.get(&c.conversation_id) {
            Some((group, _)) if *group == c.mls_group_id => by_group.entry(group.clone()).or_default().push(c),
            Some(_) => plan.deferred.push(c.conversation_id.clone()),
            // Not a member here: nothing to send, nothing to promise.
            None => {}
        }
    }
    for (group, convs) in by_group {
        let caught_up = crate::commands::messages::catch_up_mls_group_interleaved(state, &group, user).await;
        let position = with_db(state, |c| Ok(crate::commands::mls::local_position(c, &group))).await?;
        for c in convs {
            let ready = caught_up.is_ok()
                && position.is_some_and(|(g, e)| (g, e) >= (c.join_generation, c.join_epoch));
            if ready {
                plan.eligible.push(c.conversation_id.clone());
            } else {
                plan.deferred.push(c.conversation_id.clone());
            }
        }
        if let Err(e) = caught_up {
            crate::diag!("[history-transfer] catch-up of {group} failed; its conversations are deferred: {e}");
        }
    }
    plan.eligible.sort();
    plan.deferred.sort();
    Ok(plan)
}

/// How many rows the plan selects right now (a progress estimate).
fn count_selected(conn: &rusqlite::Connection, req: &wire::Req, plan: &Plan) -> Result<i64> {
    let eligible = serde_json::to_string(&plan.eligible)?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM message
         WHERE undecryptable IS NULL AND send_state IS NULL
           AND conversation_id IN (SELECT value FROM json_each(?1))
           AND (?2 <= 0 OR received_at >= datetime('now', '-' || ?2 || ' days'))",
        rusqlite::params![eligible, req.retention_days],
        |r| r.get(0),
    )?)
}

/// `(conversation_id, sent_at, id)` of the last row sent.
type Cursor = (String, String, String);

/// Seal the next chunk and store it with the advanced position, in one
/// transaction. Returns `(index, sealed)`.
fn seal_next_chunk(conn: &rusqlite::Connection, row: &LocalRow, req: &wire::Req, plan: &Plan) -> Result<(u32, Vec<u8>)> {
    let key = row.transfer_key.as_ref().ok_or_else(|| err("transfer key missing"))?;
    let cursor: Cursor = match &row.cursor {
        Some(s) => serde_json::from_str(s).map_err(|_| err("stored cursor is corrupt"))?,
        None => (String::new(), String::new(), String::new()),
    };
    let eligible = serde_json::to_string(&plan.eligible)?;
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, sender_id, content, reply_to_id, thread_id, sent_at, received_at,
                edited_at, deleted_at
         FROM message
         WHERE undecryptable IS NULL AND send_state IS NULL
           AND conversation_id IN (SELECT value FROM json_each(?1))
           AND (?2 <= 0 OR received_at >= datetime('now', '-' || ?2 || ' days'))
           AND (conversation_id, sent_at, id) > (?3, ?4, ?5)
         ORDER BY conversation_id, sent_at, id
         LIMIT ?6",
    )?;
    let candidates: Vec<wire::WireRow> = stmt
        .query_map(
            rusqlite::params![eligible, req.retention_days, cursor.0, cursor.1, cursor.2, ROWS_PER_QUERY],
            |r| {
                Ok(wire::WireRow {
                    id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    sender_id: r.get(2)?,
                    content: r.get(3)?,
                    reply_to_id: r.get(4)?,
                    thread_id: r.get(5)?,
                    sent_at: r.get(6)?,
                    received_at: r.get(7)?,
                    edited_at: r.get(8)?,
                    deleted_at: r.get(9)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut rows = Vec::new();
    let mut size = 0usize;
    let mut skipped = 0u64;
    let mut last: Option<Cursor> = None;
    for r in candidates {
        let len = serde_json::to_vec(&r)?.len() + 1;
        if len > wire::PLAIN_TARGET_BYTES || r.content.as_deref().is_some_and(|c| c.len() > wire::MAX_CONTENT_BYTES) {
            // Unsendable on its own: counted, passed.
            skipped += 1;
            last = Some((r.conversation_id.clone(), r.sent_at.clone(), r.id.clone()));
            continue;
        }
        if size + len > wire::PLAIN_TARGET_BYTES || rows.len() >= wire::MAX_ROWS {
            break;
        }
        size += len;
        last = Some((r.conversation_id.clone(), r.sent_at.clone(), r.id.clone()));
        rows.push(r);
    }
    let index = row.next_index;
    let is_final = last.is_none();

    let mut receipts = Vec::new();
    if !rows.is_empty() {
        let ids = serde_json::to_string(&rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>())?;
        let mut stmt = conn.prepare(
            "SELECT message_id, reader_id, kind, at FROM message_receipt
             WHERE message_id IN (SELECT value FROM json_each(?1)) ORDER BY message_id, reader_id, kind",
        )?;
        let all: Vec<wire::WireReceipt> = stmt
            .query_map(rusqlite::params![ids], |r| {
                Ok(wire::WireReceipt { message_id: r.get(0)?, reader_id: r.get(1)?, kind: r.get(2)?, at: r.get(3)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut rsize = 0usize;
        for rc in all {
            let len = serde_json::to_vec(&rc)?.len() + 1;
            if rsize + len > RECEIPT_BYTES || receipts.len() >= wire::MAX_RECEIPTS {
                break;
            }
            rsize += len;
            receipts.push(rc);
        }
    }

    let rows_after = row.rows_done + rows.len() as u64;
    let skipped_after = row.rows_skipped + skipped;
    let fin = is_final.then(|| wire::FinalInfo {
        total_chunks: index + 1,
        total_rows: rows_after,
        source_skipped: skipped_after,
        deferred: plan.deferred.clone(),
    });
    let plain = serde_json::to_vec(&wire::ChunkPlain {
        v: wire::VERSION,
        transfer_id: row.transfer_id.clone(),
        index,
        prev_hash: hex::encode(row.prev_hash),
        rows,
        receipts,
        fin,
        estimated_total: if index == 0 { row.estimated_total } else { None },
    })?;
    let sealed = crypto::seal(key, &row.transfer_id, index, is_final, &plain);
    let cursor_after = match last {
        Some(c) => Some(serde_json::to_string(&c)?),
        None => row.cursor.clone(),
    };

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO history_transfer_outgoing (transfer_id, idx, sealed) VALUES (?1, ?2, ?3)",
        rusqlite::params![row.transfer_id, index, sealed],
    )?;
    tx.execute(
        "UPDATE history_transfer_local
         SET next_index = ?2, cursor = ?3, prev_hash = ?4, rows_done = ?5, rows_skipped = ?6,
             final_index = ?7
         WHERE transfer_id = ?1 AND next_index = ?8",
        rusqlite::params![
            row.transfer_id,
            index + 1,
            cursor_after,
            crypto::sha256(&plain).to_vec(),
            rows_after as i64,
            skipped_after as i64,
            is_final.then_some(index),
            index
        ],
    )?;
    store::touch(&tx, &row.transfer_id)?;
    tx.commit()?;
    Ok((index, sealed))
}

enum Put {
    Stored(i64),
    WindowFull,
    Refused(String),
    Gone,
}

async fn put(state: &Arc<AppState>, id: &str, idx: u32, sealed: &[u8]) -> Result<Put> {
    let body = pollis_api::history_transfer::PutChunkBody {
        transfer_id: id.to_string(),
        idx: idx as i64,
        sealed: super::b64(sealed),
    };
    Ok(match ds(state, &body).await? {
        Ds::Ok(r) => Put::Stored(r.acked_through),
        Ds::Conflict(t) if t.contains("window_full") => Put::WindowFull,
        Ds::Conflict(t) => Put::Refused(t),
        Ds::TooMany => Put::WindowFull,
        Ds::NotFound => Put::Gone,
    })
}

async fn run_source(
    state: &Arc<AppState>,
    user: &str,
    device: &str,
    id: &str,
    sink: &Arc<dyn EventSink<HistoryTransfer>>,
) -> Result<HistoryTransfer> {
    let mut waiter = Waiter::new();
    with_db(state, |c| store::set_open_state(c, id, State::Transferring, None)).await?;
    emit(state, id, sink).await?;
    loop {
        let row = load(state, id).await?;
        if row.state.is_ended() {
            return emit(state, id, sink).await;
        }
        let st = match ds(state, &pollis_api::history_transfer::TransferStatusBody { transfer_id: id.to_string() }).await? {
            Ds::Ok(s) => s,
            Ds::NotFound => {
                end(state, id, State::Expired, Some("The other device stopped responding.")).await?;
                return emit(state, id, sink).await;
            }
            Ds::Conflict(_) | Ds::TooMany => {
                if waiter.wait(false).await {
                    return pause(state, id, sink).await;
                }
                continue;
            }
        };
        if st.src_device_id != device || st.dst_device_id != row.peer_device_id {
            return fail(state, id, "the DS answered for another transfer", sink).await;
        }
        use pollis_api::history_transfer::TransferState as Ts;
        match st.state {
            Ts::Completed => {
                end(state, id, State::Completed, None).await?;
                return emit(state, id, sink).await;
            }
            Ts::Cancelled | Ts::Declined => {
                end(state, id, State::Cancelled, Some("Cancelled on the other device.")).await?;
                return emit(state, id, sink).await;
            }
            Ts::Requested => {
                // Our accept never landed (killed in between): send it again.
                let acc = row.acc.clone().ok_or_else(|| err("stored acceptance missing"))?;
                let sig = super::sign(state, user, device, crypto::ACC_DOMAIN, &acc).await?;
                let body = pollis_api::history_transfer::AcceptTransferBody {
                    transfer_id: id.to_string(),
                    acc: super::b64(&acc),
                    acc_sig: super::b64(&sig),
                };
                if let Ds::NotFound = ds(state, &body).await? {
                    end(state, id, State::Expired, None).await?;
                    return emit(state, id, sink).await;
                }
                continue;
            }
            Ts::Accepted => {}
        }
        let mut progressed = with_db(state, |c| store::note_acked(c, id, st.acked_through)).await?;
        if let Some(fin) = row.final_index {
            if st.acked_through >= fin as i64 {
                end(state, id, State::Completed, None).await?;
                return emit(state, id, sink).await;
            }
        }
        // Re-upload, verbatim, what this device stored and the DS does not hold.
        let have = st.stored_through.max(st.acked_through);
        let stored = with_db(state, |c| store::outgoing(c, id)).await?;
        let mut blocked = false;
        for (idx, sealed) in stored.iter().filter(|(i, _)| *i as i64 > have) {
            match put(state, id, *idx, sealed).await? {
                Put::Stored(acked) => {
                    with_db(state, |c| store::note_acked(c, id, acked)).await?;
                    progressed = true;
                }
                Put::WindowFull => {
                    blocked = true;
                    break;
                }
                Put::Refused(t) => return fail(state, id, &format!("the DS refused a stored chunk: {t}"), sink).await,
                Put::Gone => break,
            }
        }
        // An index below next_index that is neither stored here nor held by
        // the DS can never be re-sealed (A3): the stream is broken.
        let stored_now = with_db(state, |c| store::outgoing(c, id)).await?;
        let highest_known = stored_now.last().map(|(i, _)| *i as i64).unwrap_or(-1).max(have);
        if !blocked && (row.next_index as i64) - 1 > highest_known {
            return fail(state, id, "a sealed chunk was lost before upload", sink).await;
        }
        let row = load(state, id).await?;
        if !blocked && row.final_index.is_none() && (row.next_index as i64) <= st.acked_through + WINDOW {
            let req = parse_req(&row)?;
            let plan: Plan = match &row.plan {
                Some(p) => serde_json::from_str(p).map_err(|_| err("stored plan is corrupt"))?,
                None => {
                    let plan = make_plan(state, user, &req).await?;
                    let json = serde_json::to_string(&plan)?;
                    with_db(state, |c| {
                        let estimate = count_selected(c, &req, &plan)?;
                        c.execute(
                            "UPDATE history_transfer_local SET plan = ?2, deferred_count = ?3, estimated_total = ?4
                             WHERE transfer_id = ?1 AND plan IS NULL",
                            rusqlite::params![id, json, plan.deferred.len() as i64, estimate],
                        )?;
                        store::touch(c, id)
                    })
                    .await?;
                    emit(state, id, sink).await?;
                    continue;
                }
            };
            let (idx, sealed) = with_db(state, |c| seal_next_chunk(c, &row, &req, &plan)).await?;
            emit(state, id, sink).await?;
            if hooks::take(Crash::TransferSourceAfterStore) {
                return Err(hooks::simulated_crash());
            }
            match put(state, id, idx, &sealed).await? {
                Put::Stored(acked) => {
                    if hooks::take(Crash::TransferSourceAfterPut) {
                        return Err(hooks::simulated_crash());
                    }
                    with_db(state, |c| store::note_acked(c, id, acked)).await?;
                }
                Put::WindowFull | Put::Gone => {}
                Put::Refused(t) => return fail(state, id, &format!("the DS refused a chunk: {t}"), sink).await,
            }
            progressed = true;
        }
        if progressed {
            emit(state, id, sink).await?;
            waiter.progressed();
        } else if waiter.wait(false).await {
            return pause(state, id, sink).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// Verify the source's acceptance and derive the key: the acceptance names this
/// transfer, this request (by hash) and these two devices, and carries the
/// source's signature under the key its cert (checked against this account's
/// own identity key) certifies.
async fn verify_acceptance(
    state: &Arc<AppState>,
    user: &str,
    device: &str,
    row: &LocalRow,
    st: &pollis_api::history_transfer::TransferStatusResponse,
) -> std::result::Result<(zeroize::Zeroizing<[u8; 32]>, Vec<u8>), String> {
    let (Some(acc_b64), Some(sig_b64)) = (st.acc.as_deref(), st.acc_sig.as_deref()) else {
        return Err("accepted without an acceptance".into());
    };
    let acc_bytes = super::unb64("acc", acc_b64).map_err(|e| e.to_string())?;
    let sig = super::unb64("acc_sig", sig_b64).map_err(|e| e.to_string())?;
    if acc_bytes.len() > 64 * 1024 {
        return Err("acceptance too large".into());
    }
    let acc: wire::Acc = serde_json::from_slice(&acc_bytes).map_err(|_| "acceptance does not parse".to_string())?;
    if acc.v != wire::VERSION
        || acc.transfer_id != row.transfer_id
        || acc.src_device_id != row.peer_device_id
        || acc.dst_device_id != device
        || acc.req_hash != hex::encode(crypto::sha256(&row.req))
    {
        return Err("acceptance names another transfer, request or device".into());
    }
    let sibling = super::verified_sibling(state, user, &row.peer_device_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("the source device is not a verified sibling")?;
    crypto::verify(&sibling.pq_pub, crypto::ACC_DOMAIN, &acc_bytes, &sig).map_err(|e| e.to_string())?;
    let secret = row.kem_sk.as_ref().ok_or("key-agreement secret missing")?;
    let ct_x = super::unb64("kem_ct_x25519", &acc.kem_ct_x25519).map_err(|e| e.to_string())?;
    let ct_k = super::unb64("kem_ct_mlkem", &acc.kem_ct_mlkem).map_err(|e| e.to_string())?;
    let shared = crypto::decapsulate(secret, &ct_x, &ct_k).map_err(|e| e.to_string())?;
    Ok((crypto::derive_key(&shared, &row.transfer_id, &row.req, &acc_bytes), acc_bytes))
}

#[derive(Debug)]
enum ImportError {
    Frame(wire::FrameError),
    Other(Error),
}

impl From<Error> for ImportError {
    fn from(e: Error) -> Self {
        ImportError::Other(e)
    }
}

impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        ImportError::Other(e.into())
    }
}

/// The receiver's upsert: a new row arrives as `origin = 'transfer'`; an
/// existing row is touched ONLY if it is a loss placeholder in the same
/// conversation (healed: content, author, dates; a tombstone on either side
/// wins). A real row of this device's own is never overwritten.
const IMPORT_SQL: &str = "INSERT INTO message
     (id, conversation_id, sender_id, ciphertext, content, reply_to_id, thread_id, sent_at,
      received_at, edited_at, deleted_at, origin)
     VALUES (?1, ?2, ?3, X'', ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'transfer')
     ON CONFLICT(id) DO UPDATE SET
       sender_id = excluded.sender_id,
       content = CASE WHEN message.deleted_at IS NULL AND excluded.deleted_at IS NULL
                      THEN excluded.content ELSE NULL END,
       reply_to_id = excluded.reply_to_id,
       thread_id = excluded.thread_id,
       sent_at = excluded.sent_at,
       received_at = excluded.received_at,
       edited_at = excluded.edited_at,
       deleted_at = COALESCE(message.deleted_at, excluded.deleted_at),
       undecryptable = NULL,
       origin = 'transfer'
     WHERE message.undecryptable IS NOT NULL AND message.conversation_id = excluded.conversation_id";

/// Deletes merge one way (A2): the source's tombstone deletes this device's
/// live copy of the same message; nothing else about a real row changes.
const DELETE_MERGE_SQL: &str = "UPDATE message SET content = NULL, deleted_at = ?3, send_state = NULL
     WHERE id = ?1 AND conversation_id = ?2 AND deleted_at IS NULL AND undecryptable IS NULL
       AND send_state IS NULL";

/// Import one checked chunk in ONE transaction and advance the receiver's
/// position with it. Returns the conversations it touched.
#[allow(clippy::too_many_arguments)]
fn import_chunk(
    conn: &rusqlite::Connection,
    row: &LocalRow,
    req: &wire::Req,
    chunk: &wire::ChunkPlain,
    plain_hash: [u8; 32],
    members: &HashMap<String, (String, bool)>,
) -> std::result::Result<Vec<String>, ImportError> {
    let signed: HashMap<&str, &wire::ReqConversation> =
        req.conversations.iter().map(|c| (c.conversation_id.as_str(), c)).collect();
    let now = chrono::Utc::now();
    let tx = conn.unchecked_transaction()?;
    let mut imported = 0u64;
    let mut skipped = 0u64;
    let mut touched: HashSet<String> = HashSet::new();
    let mut accepted: HashMap<String, bool> = HashMap::new();
    let mut held: HashMap<String, bool> = HashMap::new();
    for r in &chunk.rows {
        let Ok(checked) = wire::check_row(r, now) else {
            skipped += 1;
            continue;
        };
        let cid = checked.row.conversation_id.as_str();
        // Only conversations this device signed for, still belongs to (per the
        // DS), under the same group, and still holds a group for.
        let (Some(rc), Some((group, is_dm))) = (signed.get(cid), members.get(cid)) else {
            skipped += 1;
            continue;
        };
        if *group != rc.mls_group_id {
            skipped += 1;
            continue;
        }
        let has_group = *held
            .entry(group.clone())
            .or_insert_with(|| crate::commands::mls::has_local_group(&tx, group));
        if !has_group {
            skipped += 1;
            continue;
        }
        let w = &checked.row;
        let changed = tx.execute(
            IMPORT_SQL,
            rusqlite::params![
                w.id,
                w.conversation_id,
                w.sender_id,
                w.content,
                w.reply_to_id,
                w.thread_id,
                w.sent_at,
                checked.received_at,
                w.edited_at,
                w.deleted_at
            ],
        )?;
        if changed > 0 {
            // New or healed: the changes that waited for it apply now (A1).
            crate::commands::messages::pending_change::apply(&tx, &w.id, &w.conversation_id)?;
        } else if let Some(deleted_at) = &w.deleted_at {
            tx.execute(DELETE_MERGE_SQL, rusqlite::params![w.id, w.conversation_id, deleted_at])?;
        }
        imported += 1;
        touched.insert(w.conversation_id.clone());
        accepted.insert(w.id.clone(), *is_dm);
    }
    for rc in &chunk.receipts {
        if !wire::receipt_ok(rc) {
            continue;
        }
        if let Some(is_dm) = accepted.get(&rc.message_id) {
            crate::commands::messages::record_transferred_receipt(&tx, &rc.message_id, &rc.reader_id, &rc.kind, &rc.at, *is_dm);
        }
    }
    let seen = row.rows_done + row.rows_skipped + chunk.rows.len() as u64;
    let (source_skipped, deferred) = match &chunk.fin {
        Some(f) => {
            if seen != f.total_rows {
                return Err(ImportError::Frame(wire::FrameError::Final));
            }
            (f.source_skipped, Some(f.deferred.len() as i64))
        }
        None => (0, None),
    };
    tx.execute(
        "UPDATE history_transfer_local
         SET next_index = ?2, prev_hash = ?3, chunks_done = ?2, rows_done = rows_done + ?4,
             rows_skipped = rows_skipped + ?5, deferred_count = COALESCE(?6, deferred_count),
             final_index = ?7, estimated_total = COALESCE(?9, estimated_total)
         WHERE transfer_id = ?1 AND next_index = ?8",
        rusqlite::params![
            row.transfer_id,
            chunk.index + 1,
            plain_hash.to_vec(),
            imported as i64,
            (skipped + source_skipped) as i64,
            deferred,
            chunk.fin.as_ref().map(|_| chunk.index),
            chunk.index,
            chunk.estimated_total.filter(|_| chunk.index == 0).and_then(|n| i64::try_from(n).ok())
        ],
    )?;
    store::touch(&tx, &row.transfer_id)?;
    if hooks::take(Crash::TransferReceiverAfterOpen) {
        // Dropping `tx` rolls the whole chunk back.
        return Err(ImportError::Other(hooks::simulated_crash()));
    }
    tx.commit()?;
    Ok(touched.into_iter().collect())
}

async fn ack(state: &Arc<AppState>, id: &str, idx: u32) -> Result<bool> {
    let body = pollis_api::history_transfer::AckChunkBody { transfer_id: id.to_string(), idx: idx as i64 };
    Ok(matches!(ds(state, &body).await?, Ds::Ok(_)))
}

/// The last step: tell the DS, converge read cursors (imported history must
/// not show as unread where the account already read it), record it.
async fn complete(state: &Arc<AppState>, user: &str, id: &str, row: &LocalRow) -> Result<()> {
    let _ = ds(state, &pollis_api::history_transfer::CompleteTransferBody { transfer_id: id.to_string() }).await?;
    end(state, id, State::Completed, None).await?;
    if let Err(e) = crate::commands::messages::converge_read_cursors(state, user).await {
        crate::diag!("[history-transfer] read-cursor convergence after import failed (non-fatal): {e}");
    }
    let ev = pollis_api::account::SecurityEventBody {
        kind: "history_transfer_completed".to_string(),
        device_id: Some(row.peer_device_id.clone()),
        metadata: Some(format!("via={}", row.via)),
        user_id: Some(user.to_string()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[history-transfer] security event (complete) failed (non-fatal): {e}");
    }
    Ok(())
}

async fn run_receiver(
    state: &Arc<AppState>,
    user: &str,
    device: &str,
    id: &str,
    sink: &Arc<dyn EventSink<HistoryTransfer>>,
) -> Result<HistoryTransfer> {
    use pollis_api::history_transfer::TransferState as Ts;
    let mut waiter = Waiter::new();
    let mut members: Option<HashMap<String, (String, bool)>> = None;
    emit(state, id, sink).await?;
    loop {
        let row = load(state, id).await?;
        if row.state.is_ended() {
            return emit(state, id, sink).await;
        }
        let st = match ds(state, &pollis_api::history_transfer::TransferStatusBody { transfer_id: id.to_string() }).await? {
            Ds::Ok(s) => s,
            Ds::NotFound => {
                let why = if row.state == State::AwaitingApproval {
                    "The request expired before it was approved."
                } else {
                    "The transfer expired."
                };
                end(state, id, State::Expired, Some(why)).await?;
                return emit(state, id, sink).await;
            }
            Ds::Conflict(_) | Ds::TooMany => {
                if waiter.wait(row.state == State::AwaitingApproval).await {
                    return emit(state, id, sink).await;
                }
                continue;
            }
        };
        if st.dst_device_id != device || st.src_device_id != row.peer_device_id || st.transfer_id != id {
            return fail(state, id, "the DS answered for another transfer", sink).await;
        }
        match (row.state, st.state) {
            (_, Ts::Declined) => {
                end(state, id, State::Declined, Some("Declined on the other device.")).await?;
                return emit(state, id, sink).await;
            }
            (_, Ts::Cancelled) => {
                end(state, id, State::Cancelled, Some("Cancelled on the other device.")).await?;
                return emit(state, id, sink).await;
            }
            (State::AwaitingApproval, Ts::Requested) => {
                if waiter.wait(true).await {
                    return emit(state, id, sink).await;
                }
                continue;
            }
            (State::AwaitingApproval, Ts::Accepted) => {
                match verify_acceptance(state, user, device, &row, &st).await {
                    Ok((key, acc)) => {
                        with_db(state, |c| {
                            c.execute(
                                "UPDATE history_transfer_local
                                 SET transfer_key = ?2, acc = ?3, kem_sk = NULL, state = 'transferring', error = NULL
                                 WHERE transfer_id = ?1 AND state = 'awaiting_approval'",
                                rusqlite::params![id, key.to_vec(), acc],
                            )?;
                            store::touch(c, id)
                        })
                        .await?;
                        emit(state, id, sink).await?;
                        waiter.progressed();
                    }
                    Err(why) => return fail(state, id, &format!("acceptance refused: {why}"), sink).await,
                }
                continue;
            }
            (State::AwaitingApproval, Ts::Completed) => {
                return fail(state, id, "the DS reports a transfer this device never received", sink).await;
            }
            (_, Ts::Requested) => {
                return fail(state, id, "the DS lost the acceptance", sink).await;
            }
            (_, Ts::Completed) => {
                if row.final_index.is_some() {
                    complete(state, user, id, &row).await?;
                    return emit(state, id, sink).await;
                }
                return fail(state, id, "the DS reports completion before the final chunk", sink).await;
            }
            (_, Ts::Accepted) => {}
        }
        if row.state == State::Paused {
            with_db(state, |c| store::set_open_state(c, id, State::Transferring, None)).await?;
            emit(state, id, sink).await?;
        }
        // An import whose ack was lost: ack it now, never import it twice.
        if row.next_index > 0 && st.acked_through < row.next_index as i64 - 1 {
            ack(state, id, row.next_index - 1).await?;
            continue;
        }
        if row.final_index.is_some() {
            complete(state, user, id, &row).await?;
            return emit(state, id, sink).await;
        }
        let idx = row.next_index;
        let body = pollis_api::history_transfer::GetChunkBody { transfer_id: id.to_string(), idx: idx as i64 };
        let sealed = match ds(state, &body).await? {
            Ds::Ok(r) => r.sealed,
            Ds::NotFound | Ds::Conflict(_) | Ds::TooMany => None,
        };
        let Some(sealed) = sealed else {
            if waiter.wait(false).await {
                return pause(state, id, sink).await;
            }
            continue;
        };
        let Ok(sealed) = super::unb64("sealed", &sealed) else {
            return fail(state, id, "a chunk is not base64", sink).await;
        };
        let key = row.transfer_key.as_ref().ok_or_else(|| err("transfer key missing"))?;
        let (plain, is_final) = match crypto::open(key, id, idx, &sealed) {
            Ok(v) => v,
            Err(e) => return fail(state, id, &format!("chunk {idx} rejected: {e}"), sink).await,
        };
        let chunk = match wire::parse_chunk(&plain, id, idx, &row.prev_hash, is_final) {
            Ok(c) => c,
            Err(e) => return fail(state, id, &format!("chunk {idx} rejected: {e}"), sink).await,
        };
        let req = parse_req(&row)?;
        // Current membership: loaded once a run, refreshed before skipping a miss.
        let need_refresh = members.is_none()
            || chunk
                .rows
                .iter()
                .any(|r| !members.as_ref().is_some_and(|m| m.contains_key(&r.conversation_id)));
        if need_refresh {
            members = Some(super::membership(state, user).await?);
        }
        let members_now = members.clone().unwrap_or_default();
        let plain_hash = crypto::sha256(&plain);
        let imported = {
            let _cross = state.cross_process_lock().await?;
            with_db(state, |c| match import_chunk(c, &row, &req, &chunk, plain_hash, &members_now) {
                Ok(t) => Ok(Ok(t)),
                Err(ImportError::Frame(f)) => Ok(Err(f)),
                Err(ImportError::Other(e)) => Err(e),
            })
            .await?
        };
        let touched = match imported {
            Ok(t) => t,
            Err(f) => return fail(state, id, &format!("chunk {idx} rejected: {f}"), sink).await,
        };
        crate::commands::messages::persisted::note(touched);
        emit(state, id, sink).await?;
        if hooks::take(Crash::TransferReceiverAfterCommit) {
            return Err(hooks::simulated_crash());
        }
        ack(state, id, idx).await?;
        waiter.progressed();
    }
}
