//! Copy message history between two of one account's enrolled devices (#1735).
//!
//! The receiver (R) asks a sibling source (S); S's user approves (with the PIN,
//! or a single tap right after S itself linked R); S sends its decrypted rows to
//! R in chunks sealed end to end, through the DS. Nothing but message rows and
//! their receipts move: no MLS state, no keys, no outbox, no ingest bookkeeping.
//!
//! Design, threat model and every invariant: `docs/history-transfer-design.md`.
//!
//! - [`crypto`]: signatures, hybrid key agreement, chunk sealing (pure).
//! - [`wire`]: the transcript and chunk formats and the receiver's checks (pure; fuzzed).
//! - [`store`]: the local transfer state.
//! - [`engine`]: the resumable source and receiver loops behind
//!   [`run_history_transfer`].

pub(crate) mod crypto;
mod engine;
#[cfg(feature = "fuzzing")]
pub mod fuzz_surface;
pub(crate) mod store;
pub mod wire;

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::sink::EventSink;
use crate::state::AppState;

pub use engine::{await_history_transfer, run_history_transfer, wake};
#[cfg(feature = "test-harness")]
pub use engine::transfer_hooks;

/// How long after this device approved a link (with the PIN) the follow-on
/// history request from that new device may be approved with a single tap.
pub(crate) const LINK_SHORTCUT_SECS: u64 = 10 * 60;

/// How far a request's `created_at` may be from this device's clock: the DS
/// keeps a request 15 minutes, plus 10 minutes of skew either way.
pub(crate) const REQUEST_FRESH_PAST_SECS: i64 = 25 * 60;
pub(crate) const REQUEST_FRESH_FUTURE_SECS: i64 = 10 * 60;

/// A transfer as the UI sees it. Field names are the bridge's (snake_case).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryTransfer {
    pub transfer_id: String,
    /// `source` or `receiver`.
    pub role: String,
    pub peer_device_id: String,
    pub peer_device_name: Option<String>,
    /// `link` or `sibling`.
    pub via: String,
    /// `awaiting_approval` | `transferring` | `paused` | `completed` |
    /// `declined` | `cancelled` | `expired` | `failed`.
    pub state: String,
    pub chunks_done: u32,
    pub rows_done: u64,
    pub rows_skipped: u64,
    /// Conversations the source was not ready to send; copy again later.
    pub deferred_conversations: u32,
    /// Rows the source expects to send, once known (the source counts them
    /// when it plans; the receiver learns it from the first chunk).
    pub estimated_total: Option<u64>,
    pub error: Option<String>,
    /// RFC 3339 with milliseconds; strictly increases on every change, so it
    /// doubles as the `since` token of [`await_history_transfer`].
    pub updated_at: String,
}

impl HistoryTransfer {
    pub(crate) fn from_row(row: &store::LocalRow) -> Self {
        HistoryTransfer {
            transfer_id: row.transfer_id.clone(),
            role: match row.role {
                store::Role::Src => "source",
                store::Role::Dst => "receiver",
            }
            .to_string(),
            peer_device_id: row.peer_device_id.clone(),
            peer_device_name: row.peer_device_name.clone(),
            via: row.via.clone(),
            state: row.state.as_str().to_string(),
            chunks_done: row.chunks_done,
            rows_done: row.rows_done,
            rows_skipped: row.rows_skipped,
            deferred_conversations: row.deferred_count + row.local_deferred,
            estimated_total: row.estimated_total,
            error: row.error.clone(),
            updated_at: row.updated_at.clone(),
        }
    }

    pub fn is_ended(&self) -> bool {
        !matches!(self.state.as_str(), "awaiting_approval" | "transferring" | "paused")
    }
}

/// A sibling device this one could copy history from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferSource {
    pub device_id: String,
    pub device_name: Option<String>,
    /// From the device name's `(platform)` suffix, when it has one.
    pub platform: Option<String>,
    pub last_active_at: Option<String>,
    /// Active in the last few minutes (the DS's `last_seen`).
    pub online: bool,
    /// Whether that device's build can send history. Unknown (`None`) until a
    /// device advertises it; an old build simply never answers a request.
    pub supports_transfer: Option<bool>,
    /// The device whose approval enrolled THIS device (recorded at finalize).
    pub approved_this_device: bool,
}

/// A request addressed to this device, verified (signature, cert, freshness).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransferRequest {
    pub transfer_id: String,
    pub requester_device_id: String,
    pub requester_device_name: Option<String>,
    /// When the requester's device cert was issued (RFC 3339): "added <date>".
    pub requester_added_at: Option<String>,
    pub via: String,
    /// False only for the link-time shortcut (this device just linked it).
    pub pin_required: bool,
    /// RFC 3339.
    pub created_at: String,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(what: &str, s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| Error::Other(anyhow::anyhow!("{what}: not base64: {e}")))
}

fn rfc3339(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn err(msg: &str) -> Error {
    Error::Other(anyhow::anyhow!("{msg}"))
}

async fn identity(state: &Arc<AppState>) -> Result<(String, String)> {
    let user = crate::commands::mls::current_user_id(state).await?;
    let device = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| err("device_id not set — sign-in incomplete"))?;
    Ok((user, device))
}

/// A DS answer, with the statuses the protocol branches on kept apart.
pub(crate) enum Ds<T> {
    Ok(T),
    NotFound,
    Conflict(String),
    TooMany,
}

pub(crate) async fn ds<B: pollis_api::ClientRequest>(state: &Arc<AppState>, body: &B) -> Result<Ds<B::Response>> {
    let resp = crate::commands::mls::ds_post(state, body).await?;
    let status = resp.status();
    if status.is_success() {
        return Ok(Ds::Ok(crate::commands::mls::decode_response::<B>(resp).await?));
    }
    let text = resp.text().await.unwrap_or_default();
    match status.as_u16() {
        404 => Ok(Ds::NotFound),
        409 => Ok(Ds::Conflict(text)),
        429 => Ok(Ds::TooMany),
        _ => Err(Error::Other(anyhow::anyhow!("{} failed ({status}): {text}", B::PATH))),
    }
}

/// This account's own identity key, from the local keystore — the pin every
/// sibling's device cert is checked against (never the DS's copy).
async fn own_account_pub(state: &Arc<AppState>, user_id: &str) -> Result<Vec<u8>> {
    use ml_dsa::Keypair as _;
    let key = crate::commands::account_identity::load_account_id_key(state, user_id).await?;
    Ok(key.verifying_key().encode().to_vec())
}

/// A sibling whose device cert verified against this account's own key.
#[derive(Debug, Clone)]
pub(crate) struct Sibling {
    pub device_id: String,
    pub name: Option<String>,
    pub pq_pub: Vec<u8>,
    pub issued_at: u64,
    pub last_seen: Option<String>,
}

/// Verify one device row: live, certified by `account_pub`, and naming the
/// keys it carries. Mirrors `IdentityDirectory::leaf_verdict`.
pub(crate) fn verify_device_row(
    account_pub: &[u8],
    row: &pollis_api::account_reads::DeviceRow,
) -> std::result::Result<Sibling, &'static str> {
    if row.revoked_at.is_some() {
        return Err("revoked");
    }
    let (Some(cert), Some(issued), Some(version), Some(ed), Some(pq)) = (
        row.device_cert.as_deref(),
        row.cert_issued_at.as_deref(),
        row.cert_identity_version,
        row.mls_signature_pub.as_deref(),
        row.mls_signature_pub_pq.as_deref(),
    ) else {
        return Err("no device cert");
    };
    let dec = |s: &str| base64::engine::general_purpose::STANDARD.decode(s).map_err(|_| "cert not base64");
    let (cert, ed, pq) = (dec(cert)?, dec(ed)?, dec(pq)?);
    let issued_at: u64 = issued.parse().map_err(|_| "cert_issued_at unparseable")?;
    let version = u32::try_from(version).map_err(|_| "bad identity version")?;
    crate::commands::account_identity::verify_device_cert(account_pub, &row.device_id, &ed, &pq, version, issued_at, &cert)
        .map_err(|_| "device cert invalid")?;
    Ok(Sibling {
        device_id: row.device_id.clone(),
        name: row.device_name.clone(),
        pq_pub: pq,
        issued_at,
        last_seen: row.last_seen.clone(),
    })
}

pub(crate) async fn verified_sibling(state: &Arc<AppState>, user_id: &str, device_id: &str) -> Result<Option<Sibling>> {
    let account_pub = own_account_pub(state, user_id).await?;
    let rows = crate::commands::ds_reads::devices(state, Some(user_id), vec![device_id.to_string()], true).await?;
    let Some(row) = rows.into_iter().find(|r| r.device_id == device_id) else {
        return Ok(None);
    };
    match verify_device_row(&account_pub, &row) {
        Ok(s) => Ok(Some(s)),
        Err(why) => {
            crate::diag!("[history-transfer] sibling {device_id} not usable: {why}");
            Ok(None)
        }
    }
}

/// Sign with this device's ML-DSA-44 key (under the local-DB lock; no await).
async fn sign(state: &Arc<AppState>, user_id: &str, device_id: &str, domain: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("not signed in"))?;
    let provider = crate::commands::mls::PollisProvider::new(db.conn());
    let (key, _) = crate::commands::mls::load_device_pq_signing_key(&provider, user_id, device_id)?;
    Ok(crypto::sign(&key, domain, bytes))
}

/// Run `f` on the local database.
async fn with_db<T>(state: &Arc<AppState>, f: impl FnOnce(&rusqlite::Connection) -> Result<T>) -> Result<T> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("not signed in"))?;
    f(db.conn())
}

fn platform_of(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    let open = name.rfind('(')?;
    let inner = name[open + 1..].strip_suffix(')')?.trim().to_ascii_lowercase();
    matches!(inner.as_str(), "ios" | "ipados" | "android" | "macos" | "windows" | "linux").then_some(inner)
}

fn active_within(last_seen: Option<&str>, secs: i64) -> bool {
    last_seen
        .and_then(crate::commands::mls::parse_ds_instant)
        .is_some_and(|t| chrono::Utc::now() - t <= chrono::Duration::seconds(secs))
}

/// The account's other live, certified devices — the ones this device could
/// copy history from. Recently active first.
pub async fn list_history_transfer_sources(state: &Arc<AppState>) -> Result<Vec<TransferSource>> {
    let (user, device) = identity(state).await?;
    let account_pub = own_account_pub(state, &user).await?;
    let rows = crate::commands::ds_reads::devices(state, Some(&user), Vec::new(), false).await?;
    let approver = with_db(state, |c| Ok(enrollment_approver(c))).await?;
    let mut out: Vec<TransferSource> = rows
        .iter()
        .filter(|r| r.device_id != device)
        .filter_map(|r| verify_device_row(&account_pub, r).ok())
        .map(|s| TransferSource {
            platform: platform_of(s.name.as_deref()),
            online: active_within(s.last_seen.as_deref(), 5 * 60),
            last_active_at: s.last_seen.clone(),
            device_name: s.name,
            approved_this_device: approver.as_deref() == Some(s.device_id.as_str()),
            device_id: s.device_id,
            supports_transfer: None,
        })
        .collect();
    out.sort_by(|a, b| {
        b.approved_this_device
            .cmp(&a.approved_this_device)
            .then_with(|| b.online.cmp(&a.online))
            .then_with(|| b.last_active_at.cmp(&a.last_active_at))
    });
    Ok(out)
}

const APPROVER_KEY: &str = "enrollment_approver_device_id";

/// Record the device that approved this one's enrollment (at finalize).
pub(crate) fn remember_enrollment_approver(conn: &rusqlite::Connection, device_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        rusqlite::params![APPROVER_KEY, device_id],
    )?;
    Ok(())
}

fn enrollment_approver(conn: &rusqlite::Connection) -> Option<String> {
    use rusqlite::OptionalExtension as _;
    conn.query_row("SELECT value FROM kv WHERE key = ?1", rusqlite::params![APPROVER_KEY], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

/// The conversations this device belongs to, each with its MLS group and
/// whether it is a DM, straight from the DS (current membership).
pub(crate) async fn membership(state: &Arc<AppState>, user_id: &str) -> Result<HashMap<String, (String, bool)>> {
    let convs = crate::commands::ds_reads::conversations(state, user_id).await?;
    let mut out = HashMap::new();
    for group in convs.group_ids {
        let resolved = crate::commands::ds_reads::catch_up(state, &group, false, false).await?;
        if !resolved.authorized {
            continue;
        }
        let mls_group = if resolved.mls_group_id.is_empty() { group.clone() } else { resolved.mls_group_id.clone() };
        for c in resolved.conversation_ids {
            out.insert(c, (mls_group.clone(), false));
        }
    }
    for dm in convs.dm_ids {
        out.insert(dm.clone(), (dm, true));
    }
    Ok(out)
}

/// Ask `source_device_id` for this device's message history.
pub async fn request_history_transfer(
    state: &Arc<AppState>,
    source_device_id: String,
    via: String,
) -> Result<HistoryTransfer> {
    if via != "link" && via != "sibling" {
        return Err(err("via must be link or sibling"));
    }
    let (user, device) = identity(state).await?;
    if source_device_id == device {
        return Err(err("A device can't copy history from itself."));
    }
    let Some(sibling) = verified_sibling(state, &user, &source_device_id).await? else {
        return Err(err("That device can't be verified as one of yours."));
    };
    // An earlier receiving transfer: still open on the DS → refuse; gone → end it here.
    if let Some(open) = with_db(state, store::open_receiving).await? {
        let alive = matches!(
            ds(state, &pollis_api::history_transfer::TransferStatusBody { transfer_id: open.clone() }).await?,
            Ds::Ok(s) if s.state.is_open()
        );
        if alive {
            return Err(err("History is already being copied to this device."));
        }
        with_db(state, |c| store::end(c, &open, store::State::Expired, None)).await?;
    }

    let members = membership(state, &user).await?;
    let (conversations, retention_days, unjoined) = with_db(state, |conn| {
        // A conversation this device belongs to but holds no group for yet (its
        // Welcome not processed — typical right after a link) cannot be asked
        // for; it is reported as deferred, never silently dropped.
        let unjoined = members
            .values()
            .filter(|(group, _)| crate::commands::mls::join_position(conn, group).is_none())
            .count();
        let mut list: Vec<wire::ReqConversation> = members
            .iter()
            .filter_map(|(cid, (group, is_dm))| {
                let (generation, epoch) = crate::commands::mls::join_position(conn, group)?;
                Some(wire::ReqConversation {
                    conversation_id: cid.clone(),
                    mls_group_id: group.clone(),
                    join_generation: generation,
                    join_epoch: epoch,
                    is_dm: *is_dm,
                })
            })
            .collect();
        list.sort_by(|a, b| a.conversation_id.cmp(&b.conversation_id));
        Ok((list, crate::db::local::get_message_retention_days(conn)?, unjoined))
    })
    .await?;

    let transfer_id = ulid::Ulid::new().to_string();
    let keys = crypto::receiver_keys();
    let req = wire::Req {
        v: wire::VERSION,
        transfer_id: transfer_id.clone(),
        user_id: user.clone(),
        dst_device_id: device.clone(),
        src_device_id: source_device_id.clone(),
        kem_pub_x25519: b64(&keys.x25519_pub),
        kem_pub_mlkem: b64(&keys.mlkem_pub),
        retention_days,
        via: via.clone(),
        created_at: crate::util::now_unix() as i64,
        conversations,
    };
    let req_bytes = serde_json::to_vec(&req)?;
    let sig = sign(state, &user, &device, crypto::REQ_DOMAIN, &req_bytes).await?;
    // Stored BEFORE the post: a crash after it leaves a row the DS knows about.
    with_db(state, |c| {
        store::insert(
            c,
            &transfer_id,
            store::Role::Dst,
            &source_device_id,
            sibling.name.as_deref(),
            &via,
            store::State::AwaitingApproval,
            Some(&keys.secret),
            None,
            &req_bytes,
            None,
        )?;
        c.execute(
            "UPDATE history_transfer_local SET local_deferred = ?2 WHERE transfer_id = ?1",
            rusqlite::params![transfer_id, unjoined as i64],
        )?;
        Ok(())
    })
    .await?;
    let body = pollis_api::history_transfer::RequestTransferBody {
        transfer_id: transfer_id.clone(),
        src_device_id: source_device_id,
        via,
        req: b64(&req_bytes),
        req_sig: b64(&sig),
    };
    let outcome = ds(state, &body).await;
    let failure = match outcome {
        Ok(Ds::Ok(_)) => None,
        Ok(Ds::NotFound) => Some((store::State::Expired, "That device is no longer available.".to_string())),
        Ok(Ds::TooMany) => Some((store::State::Failed, "Too many requests — try again later.".to_string())),
        Ok(Ds::Conflict(t)) => Some((store::State::Failed, format!("The request was refused: {t}"))),
        Err(e) => Some((store::State::Failed, e.to_string())),
    };
    if let Some((ended, why)) = failure {
        with_db(state, |c| store::end(c, &transfer_id, ended, Some(&why))).await?;
        return Err(err(&why));
    }
    snapshot(state, &transfer_id).await
}

/// A request read from the DS, fully verified.
struct VerifiedRequest {
    pending: pollis_api::history_transfer::PendingTransfer,
    req: wire::Req,
    req_bytes: Vec<u8>,
    requester: Sibling,
}

/// The DS's pending requests for this device, each verified: the receiver's
/// signature against its cert, the cert against this account's own key, the
/// request naming this account and this device, fresh, and not handled here
/// before. Anything failing is logged and never shown.
async fn verified_pending(state: &Arc<AppState>) -> Result<Vec<VerifiedRequest>> {
    let (user, device) = identity(state).await?;
    let pending = match ds(state, &pollis_api::history_transfer::PendingTransfersBody {}).await? {
        Ds::Ok(p) => p.requests,
        _ => return Ok(Vec::new()),
    };
    let account_pub = own_account_pub(state, &user).await?;
    let now = crate::util::now_unix() as i64;
    let mut out = Vec::new();
    for p in pending {
        let checked: std::result::Result<VerifiedRequest, String> = async {
            let req_bytes = unb64("req", &p.req).map_err(|e| e.to_string())?;
            let sig = unb64("req_sig", &p.req_sig).map_err(|e| e.to_string())?;
            if req_bytes.len() > 256 * 1024 {
                return Err("request too large".to_string());
            }
            let req: wire::Req = serde_json::from_slice(&req_bytes).map_err(|_| "request does not parse".to_string())?;
            if !wire::req_ok(&req) {
                return Err("request out of bounds".into());
            }
            if req.transfer_id != p.transfer_id
                || req.user_id != user
                || req.src_device_id != device
                || req.dst_device_id != p.dst_device_id
                || req.via != p.via
                || req.dst_device_id == device
            {
                return Err("request names another transfer, account or device".into());
            }
            if req.created_at < now - REQUEST_FRESH_PAST_SECS || req.created_at > now + REQUEST_FRESH_FUTURE_SECS {
                return Err("request is stale".into());
            }
            let rows = crate::commands::ds_reads::devices(state, Some(&user), vec![req.dst_device_id.clone()], true)
                .await
                .map_err(|e| e.to_string())?;
            let row = rows.into_iter().find(|r| r.device_id == req.dst_device_id).ok_or("requester unknown")?;
            let requester = verify_device_row(&account_pub, &row).map_err(str::to_string)?;
            crypto::verify(&requester.pq_pub, crypto::REQ_DOMAIN, &req_bytes, &sig).map_err(|e| e.to_string())?;
            Ok(VerifiedRequest { pending: p.clone(), req, req_bytes, requester })
        }
        .await;
        match checked {
            Ok(v) => out.push(v),
            Err(why) => crate::diag!("[history-transfer] request {} refused: {why}", p.transfer_id),
        }
    }
    // Handled here before (approved, declined, or ended): never shown again.
    let handled: Vec<bool> = with_db(state, |c| {
        out.iter().map(|v| Ok(store::load(c, &v.pending.transfer_id)?.is_some())).collect()
    })
    .await?;
    Ok(out.into_iter().zip(handled).filter(|(_, h)| !h).map(|(v, _)| v).collect())
}

/// Record that this device just approved `new_device_id`'s link with the PIN,
/// for the link-time single-tap approval (#1735). Called only by the
/// PIN-verified QR link flows (`approve_device_link`), never by the typed-code
/// enrollment approval.
pub(crate) async fn note_link_approved(state: &Arc<AppState>, new_device_id: &str) {
    state
        .history_link_approvals
        .lock()
        .await
        .insert(new_device_id.to_string(), std::time::Instant::now());
}

async fn link_shortcut(state: &Arc<AppState>, requester: &str, via: &str) -> bool {
    if via != "link" {
        return false;
    }
    state
        .history_link_approvals
        .lock()
        .await
        .get(requester)
        .is_some_and(|t| t.elapsed().as_secs() < LINK_SHORTCUT_SECS)
}

/// Requests from this account's other devices waiting for this device's approval.
pub async fn list_pending_history_transfer_requests(state: &Arc<AppState>) -> Result<Vec<PendingTransferRequest>> {
    let verified = verified_pending(state).await?;
    let mut out = Vec::with_capacity(verified.len());
    for v in verified {
        let pin_required = !link_shortcut(state, &v.req.dst_device_id, &v.req.via).await;
        out.push(PendingTransferRequest {
            transfer_id: v.pending.transfer_id.clone(),
            requester_device_id: v.req.dst_device_id.clone(),
            requester_device_name: v.requester.name.clone(),
            requester_added_at: Some(rfc3339(v.requester.issued_at as i64)),
            via: v.req.via.clone(),
            pin_required,
            created_at: rfc3339(v.pending.created_at),
        });
    }
    Ok(out)
}

/// Approve a request: verify it again, check the PIN (unless the link-time
/// shortcut applies), agree the key, store everything, then tell the DS.
pub async fn approve_history_transfer(
    state: &Arc<AppState>,
    transfer_id: String,
    pin: Option<String>,
) -> Result<HistoryTransfer> {
    let (user, device) = identity(state).await?;
    let v = verified_pending(state)
        .await?
        .into_iter()
        .find(|v| v.pending.transfer_id == transfer_id)
        .ok_or_else(|| err("This request is no longer waiting for approval."))?;
    let shortcut = link_shortcut(state, &v.req.dst_device_id, &v.req.via).await;
    match (shortcut, pin) {
        (_, Some(pin)) => crate::commands::pin::verify_pin(state, &user, &pin).await?,
        (true, None) => {}
        (false, None) => return Err(err("Enter your PIN to approve.")),
    }
    // The link-time shortcut is good for ONE approval.
    state.history_link_approvals.lock().await.remove(&v.req.dst_device_id);
    let x_pub = unb64("kem_pub_x25519", &v.req.kem_pub_x25519)?;
    let k_pub = unb64("kem_pub_mlkem", &v.req.kem_pub_mlkem)?;
    let enc = crypto::encapsulate(&x_pub, &k_pub).map_err(|e| err(&format!("request keys unusable: {e}")))?;
    let acc = wire::Acc {
        v: wire::VERSION,
        transfer_id: transfer_id.clone(),
        src_device_id: device.clone(),
        dst_device_id: v.req.dst_device_id.clone(),
        req_hash: hex::encode(crypto::sha256(&v.req_bytes)),
        kem_ct_x25519: b64(&enc.x25519_ct),
        kem_ct_mlkem: b64(&enc.mlkem_ct),
    };
    let acc_bytes = serde_json::to_vec(&acc)?;
    let acc_sig = sign(state, &user, &device, crypto::ACC_DOMAIN, &acc_bytes).await?;
    let key = crypto::derive_key(&enc.shared, &transfer_id, &v.req_bytes, &acc_bytes);
    // Stored BEFORE the accept, with the key: a crash after it resumes.
    with_db(state, |c| {
        store::insert(
            c,
            &transfer_id,
            store::Role::Src,
            &v.req.dst_device_id,
            v.requester.name.as_deref(),
            &v.req.via,
            store::State::Transferring,
            None,
            Some(&key),
            &v.req_bytes,
            Some(&acc_bytes),
        )
    })
    .await?;
    let body = pollis_api::history_transfer::AcceptTransferBody {
        transfer_id: transfer_id.clone(),
        acc: b64(&acc_bytes),
        acc_sig: b64(&acc_sig),
    };
    let ended = match ds(state, &body).await {
        Ok(Ds::Ok(_)) => None,
        Ok(Ds::NotFound) => Some((store::State::Expired, "This request expired.".to_string())),
        Ok(Ds::Conflict(_)) | Ok(Ds::TooMany) => Some((store::State::Failed, "The request can no longer be approved.".to_string())),
        Err(e) => Some((store::State::Failed, e.to_string())),
    };
    if let Some((s, why)) = ended {
        with_db(state, |c| store::end(c, &transfer_id, s, Some(&why))).await?;
        return Err(err(&why));
    }
    let ev = pollis_api::account::SecurityEventBody {
        kind: "history_transfer_approved".to_string(),
        device_id: Some(v.req.dst_device_id.clone()),
        metadata: Some(format!("via={}", v.req.via)),
        user_id: Some(user.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[history-transfer] security event (approve) failed (non-fatal): {e}");
    }
    snapshot(state, &transfer_id).await
}

/// Decline a request. Remembered here, so the same request is never shown again.
pub async fn decline_history_transfer(state: &Arc<AppState>, transfer_id: String) -> Result<()> {
    let found = verified_pending(state).await?.into_iter().find(|v| v.pending.transfer_id == transfer_id);
    match ds(state, &pollis_api::history_transfer::DeclineTransferBody { transfer_id: transfer_id.clone() }).await? {
        Ds::Ok(_) | Ds::NotFound => {}
        Ds::Conflict(t) => return Err(err(&format!("Could not decline: {t}"))),
        Ds::TooMany => return Err(err("Too many requests — try again.")),
    }
    if let Some(v) = found {
        with_db(state, |c| {
            if store::load(c, &transfer_id)?.is_none() {
                store::insert(
                    c,
                    &transfer_id,
                    store::Role::Src,
                    &v.req.dst_device_id,
                    v.requester.name.as_deref(),
                    &v.req.via,
                    store::State::Declined,
                    None,
                    None,
                    &v.req_bytes,
                    None,
                )?;
            }
            Ok(())
        })
        .await?;
    }
    Ok(())
}

/// Cancel a transfer from either side. Ends it locally whatever the DS says.
pub async fn cancel_history_transfer(state: &Arc<AppState>, transfer_id: String) -> Result<()> {
    let exists = with_db(state, |c| store::load(c, &transfer_id)).await?.is_some();
    if !exists {
        return Err(err("No such transfer."));
    }
    match ds(state, &pollis_api::history_transfer::CancelTransferBody { transfer_id: transfer_id.clone() }).await {
        Ok(_) => {}
        Err(e) => crate::diag!("[history-transfer] cancel {transfer_id}: DS not told ({e}); ended locally"),
    }
    with_db(state, |c| store::end(c, &transfer_id, store::State::Cancelled, None)).await?;
    engine::notify_progress();
    // A run waiting on this transfer re-reads it now and stops.
    engine::wake();
    Ok(())
}

/// The recent transfers on this device, newest first.
pub async fn list_history_transfers(state: &Arc<AppState>) -> Result<Vec<HistoryTransfer>> {
    with_db(state, |c| Ok(store::list(c, 20)?.iter().map(HistoryTransfer::from_row).collect())).await
}

pub(crate) async fn snapshot(state: &Arc<AppState>, transfer_id: &str) -> Result<HistoryTransfer> {
    with_db(state, |c| {
        store::load(c, transfer_id)?.map(|r| HistoryTransfer::from_row(&r)).ok_or_else(|| err("No such transfer."))
    })
    .await
}

/// After unlock: resume every open transfer in the background, each on its own
/// task (a receiver still awaiting approval must not hold up the others). Runs
/// on desktop and mobile alike (the unlock is pollis-core's); the mobile host's
/// own runner calling `run_history_transfer` too is harmless — a second run of
/// a transfer follows the first.
pub fn spawn_resume_all(state: &Arc<AppState>) {
    if !engine::auto_resume_enabled() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let ids = match with_db(&state, |c| {
            store::prune(c);
            store::open_ids(c)
        })
        .await
        {
            Ok(ids) => ids,
            Err(_) => return,
        };
        for id in ids {
            let state = state.clone();
            tokio::spawn(async move {
                let sink: Arc<dyn EventSink<HistoryTransfer>> = Arc::new(crate::sink::NoopSink);
                if let Err(e) = run_history_transfer(&state, id.clone(), sink).await {
                    crate::diag!("[history-transfer] resume {id}: {e}");
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_comes_only_from_a_known_suffix() {
        assert_eq!(platform_of(Some("Dan's MacBook (macos)")).as_deref(), Some("macos"));
        assert_eq!(platform_of(Some("Pixel (Android)")).as_deref(), Some("android"));
        assert_eq!(platform_of(Some("Box (toaster)")), None);
        assert_eq!(platform_of(Some("no suffix")), None);
        assert_eq!(platform_of(None), None);
    }
}
