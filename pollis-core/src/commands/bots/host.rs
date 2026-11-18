//! The host side of a bot (#1182): pairing a fresh host, adding a host to an
//! existing bot, and — on a host already running — verifying and acting on a
//! host approval. `pollis-bot` (the runtime) is the caller; nothing here loops
//! forever or polls on a timer.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use pollis_api::bots::{
    BotPairingRequestBody, BotPairingStatus, BotPairingStatusBody, PairingTarget, PendingBotHostWire,
    PendingBotHostsBody, WrapBotHostBody,
};

use super::approval::{self, ApprovalDecodeError, HostApproval, HOST_APPROVAL_MAX_TTL_SECS, HOST_APPROVAL_VERSION};
use super::code::{code_lookup, derive_pairing_code};
use super::unattended::{self, BotSession, Installation};
use crate::commands::transparency::AuditStatus;
use crate::error::{Error, Result};
use crate::state::AppState;

/// First delay after a `pending` answer while awaiting a pairing.
const AWAIT_BACKOFF_START_MS: u64 = 1_000;
/// The longest delay between two reads while awaiting a pairing.
const AWAIT_BACKOFF_MAX_MS: u64 = 8_000;
/// A host whose device was seen this recently counts as online for the wrap rule.
const ONLINE_WINDOW_SECS: i64 = 15 * 60;
/// How long each host ranked below the lowest waits before wrapping in its place.
pub const WRAP_STAGGER_SECS: i64 = 30;
/// Clock skew tolerated on an approval's `issued_at`.
const ISSUED_AT_SKEW_SECS: i64 = 60;
/// How many fresh ephemeral keys to try when the DS says a code is taken.
const CODE_TAKEN_RETRIES: usize = 3;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn err(msg: impl std::fmt::Display) -> Error {
    Error::Other(anyhow::anyhow!("{msg}"))
}

// ------------------------------------------------------------------ pairing

/// An open pairing request on this host. Holds the request's secrets in memory
/// only: a host restarted before approval starts again with a new code.
pub struct PairingHandle {
    /// The DS's id for the request.
    pub request_id: String,
    /// The eight-character code to print for the admin.
    pub code: String,
    /// Unix seconds.
    pub expires_at: i64,
    /// The device id this host will have under the bot.
    pub device_id: String,
    /// `None` for a new bot, the bot's id when adding a host.
    pub existing_bot_user_id: Option<String>,
    ephemeral_secret: Zeroizing<[u8; 32]>,
    poll_secret: Zeroizing<[u8; 32]>,
    /// The new bot's account seed, generated here; `None` when adding a host.
    account_seed: Option<Zeroizing<[u8; 32]>>,
}

impl std::fmt::Debug for PairingHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingHandle")
            .field("request_id", &self.request_id)
            .field("code", &self.code)
            .field("expires_at", &self.expires_at)
            .field("device_id", &self.device_id)
            .field("existing_bot_user_id", &self.existing_bot_user_id)
            .finish_non_exhaustive()
    }
}

/// How awaiting a pairing ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingOutcome {
    /// The bot exists with this host as its device: cert and key packages are
    /// published and the installation is saved for unattended starts. Next:
    /// [`join_bot_group`].
    Paired(BotSession),
    /// Nobody approved it within its TTL.
    Expired,
}

fn account_pub_of(seed: &[u8; 32]) -> Vec<u8> {
    use ml_dsa::Keypair as _;
    crate::commands::account_identity::AccountSigningKey::from_seed(&(*seed).into())
        .verifying_key()
        .encode()
        .to_vec()
}

/// A new bot: generate its account identity key and this host's device id
/// locally (they never leave the host), file the pairing request, and return
/// the code to print.
pub async fn start_new_bot_pairing(state: &Arc<AppState>, device_name: Option<String>) -> Result<PairingHandle> {
    let mut seed = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(&mut *seed);
    let account_id_pub = account_pub_of(&seed);
    let target = PairingTarget::NewBot { account_id_pub: b64().encode(&account_id_pub) };
    file_pairing_request(state, device_name, target, None, Some(seed)).await
}

/// Another host for an existing bot: file a request naming it. The bot's
/// existing hosts wrap its account key to this host once an admin approves.
pub async fn start_add_host_pairing(
    state: &Arc<AppState>,
    bot_user_id: &str,
    device_name: Option<String>,
) -> Result<PairingHandle> {
    let target = PairingTarget::ExistingBot { bot_user_id: bot_user_id.to_string() };
    file_pairing_request(state, device_name, target, Some(bot_user_id.to_string()), None).await
}

async fn file_pairing_request(
    state: &Arc<AppState>,
    device_name: Option<String>,
    target: PairingTarget,
    existing_bot_user_id: Option<String>,
    account_seed: Option<Zeroizing<[u8; 32]>>,
) -> Result<PairingHandle> {
    let device_id = ulid::Ulid::new().to_string();
    for _ in 0..CODE_TAKEN_RETRIES {
        let mut ephemeral_secret = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(&mut *ephemeral_secret);
        let ephemeral_pub = PublicKey::from(&StaticSecret::from(*ephemeral_secret));
        let mut poll_secret = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(&mut *poll_secret);

        let code = derive_pairing_code(ephemeral_pub.as_bytes(), &device_id, &target)?;
        let body = BotPairingRequestBody {
            code_lookup: b64().encode(code_lookup(&code)),
            poll_verifier: b64().encode(Sha256::digest(poll_secret.as_slice())),
            ephemeral_pub: b64().encode(ephemeral_pub.as_bytes()),
            device_id: device_id.clone(),
            device_name: device_name.clone(),
            target: target.clone(),
        };
        let resp = crate::commands::mls::ds_post_plain(state, &body).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::CONFLICT {
            crate::diag!("[bots] pairing code collided with another open request; drawing a new key");
            continue;
        }
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            return Err(err(format!("bot pairing request refused ({status}): {txt}")));
        }
        let created = crate::commands::mls::decode_response::<BotPairingRequestBody>(resp).await?;
        crate::diag!("[bots] pairing request {} filed; waiting for an admin", created.request_id);
        return Ok(PairingHandle {
            request_id: created.request_id,
            code,
            expires_at: created.expires_at,
            device_id,
            existing_bot_user_id,
            ephemeral_secret,
            poll_secret,
            account_seed,
        });
    }
    Err(err("could not file a pairing request: every code drawn was already in use; try again"))
}

/// Read the request's status once. `None`: unknown or expired.
async fn pairing_status(state: &Arc<AppState>, handle: &PairingHandle) -> Result<Option<BotPairingStatus>> {
    let body = BotPairingStatusBody {
        request_id: handle.request_id.clone(),
        poll_secret: b64().encode(handle.poll_secret.as_slice()),
    };
    let resp = crate::commands::mls::ds_post_plain(state, &body).await?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(err(format!("bot pairing status ({status}): {txt}")));
    }
    Ok(Some(crate::commands::mls::decode_response::<BotPairingStatusBody>(resp).await?))
}

/// Wait for an admin to act on the request, then install this host.
///
/// ONE call, like `await_enrollment_approval`: the core backs off between reads
/// and stops at the request's TTL. On approval it installs the bot here:
/// local store, device cert, key packages, keystore slot.
pub async fn await_pairing(state: &Arc<AppState>, handle: &PairingHandle) -> Result<PairingOutcome> {
    let mut delay = Duration::from_millis(AWAIT_BACKOFF_START_MS);
    loop {
        let status = pairing_status(state, handle).await?;
        match status {
            None => return Ok(PairingOutcome::Expired),
            Some(BotPairingStatus::Pending) => {}
            Some(BotPairingStatus::Created { bot_user_id, group_id }) => {
                let Some(seed) = handle.account_seed.as_ref() else {
                    return Err(err("the DS answered a new-bot outcome to an add-host request"));
                };
                return install(state, seed, &bot_user_id, &group_id, &handle.device_id)
                    .await
                    .map(PairingOutcome::Paired);
            }
            Some(BotPairingStatus::Wrapped { bot_user_id, group_id, wrapped_account_key, wrapped_by_device_id }) => {
                if handle.existing_bot_user_id.as_deref() != Some(bot_user_id.as_str()) {
                    return Err(err("the DS answered an add-host outcome for a different bot"));
                }
                let wrapped = crate::commands::ds_reads::decode_b64("wrapped_account_key", &wrapped_account_key)?;
                let seed_vec = Zeroizing::new(crate::commands::device_enrollment::unwrap_account_key(
                    &wrapped,
                    &*handle.ephemeral_secret,
                )?);
                let seed: Zeroizing<[u8; 32]> = Zeroizing::new(
                    seed_vec
                        .as_slice()
                        .try_into()
                        .map_err(|_| err("the unwrapped account key is not 32 bytes"))?,
                );
                crate::diag!("[bots] host {} received the account key from host {wrapped_by_device_id}", handle.device_id);
                return install(state, &seed, &bot_user_id, &group_id, &handle.device_id)
                    .await
                    .map(PairingOutcome::Paired);
            }
        }
        let now = chrono::Utc::now().timestamp();
        if now + delay.as_secs() as i64 > handle.expires_at + 5 {
            return Ok(PairingOutcome::Expired);
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, Duration::from_millis(AWAIT_BACKOFF_MAX_MS));
    }
}

/// Make this host a device of `bot_user_id`: open its store, publish its cert
/// and key packages, check the DS reports the account key it holds, and save
/// the installation for unattended starts. Undone on any failure.
async fn install(
    state: &Arc<AppState>,
    seed: &[u8; 32],
    bot_user_id: &str,
    group_id: &str,
    device_id: &str,
) -> Result<BotSession> {
    if !crate::util::is_safe_id(bot_user_id) || !crate::util::is_safe_id(group_id) {
        return Err(err("the DS named a malformed bot or group id"));
    }
    let mut db_key = Zeroizing::new(vec![0u8; 32]);
    OsRng.fill_bytes(&mut db_key);
    let inst = Installation {
        session: BotSession {
            bot_user_id: bot_user_id.to_string(),
            group_id: group_id.to_string(),
            device_id: device_id.to_string(),
        },
        account_seed: Zeroizing::new(seed.to_vec()),
        db_key,
    };
    unattended::activate(state, &inst).await?;
    match finish_install(state, &inst, seed).await {
        Ok(()) => Ok(inst.session),
        Err(e) => {
            unattended::deactivate(state).await;
            crate::db::local::remove_user_database(&state.local_data_dir(), bot_user_id);
            Err(e)
        }
    }
}

async fn finish_install(state: &Arc<AppState>, inst: &Installation, seed: &[u8; 32]) -> Result<()> {
    let user_id = &inst.session.bot_user_id;
    let device_id = &inst.session.device_id;
    let account_pub = account_pub_of(seed);
    crate::commands::account_identity::remember_published_account_id_pub(state, user_id, &account_pub).await?;

    // The cert-validity gate refuses a cert under any other key, so a published cert
    // already means the DS holds ours; the signed read below checks it from this side.
    if !crate::commands::mls::ensure_device_cert(state, user_id, device_id).await? {
        return Err(err("could not certify this host's device"));
    }
    let (published, _) = crate::commands::ds_reads::account_key(state, user_id).await?;
    if published != account_pub {
        return Err(err(
            "tampering: the DS reports a different account key for this bot than the one this host holds",
        ));
    }
    unattended::confirm_is_bot(state, user_id).await?;
    crate::commands::mls::ensure_mls_key_package(state, user_id, device_id).await?;
    unattended::persist(state, inst).await?;
    crate::diag!("[bots] host {device_id} installed as a device of bot {user_id}");
    Ok(())
}

/// Bring this host into its bot's group: drain Welcomes (an admin's reconcile
/// adds a host with key packages), backing off up to `wait`; if none came,
/// external-join, as any new device does at the end of enrollment.
pub async fn join_bot_group(state: &Arc<AppState>, session: &BotSession, wait: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + wait;
    let mut delay = Duration::from_millis(AWAIT_BACKOFF_START_MS);
    loop {
        crate::commands::mls::poll_mls_welcomes_inner(state, &session.bot_user_id, &session.device_id).await?;
        if has_group(state, &session.group_id).await {
            return Ok(());
        }
        if std::time::Instant::now() + delay >= deadline {
            break;
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, Duration::from_millis(AWAIT_BACKOFF_MAX_MS));
    }
    crate::diag!("[bots] no Welcome for {} yet; joining its group directly", session.bot_user_id);
    crate::commands::mls::external_join_group(state, &session.group_id, &session.bot_user_id).await
}

async fn has_group(state: &Arc<AppState>, group_id: &str) -> bool {
    let guard = state.local_db.lock().await;
    guard
        .as_ref()
        .is_some_and(|db| crate::commands::mls::has_local_group(db.conn(), group_id))
}

// ------------------------------------------------------------------ host approvals

/// The transparency-log check on the approver's account key. A trait so the
/// flows harness, which has no log, can say what the log would.
#[async_trait::async_trait]
pub trait AccountKeyAudit: Send + Sync {
    async fn audit(&self, state: &Arc<AppState>, user_id: &str) -> AuditStatus;
}

/// The real check: the published account-key log (`transparency::audit_peer_account_key`).
pub struct TransparencyLogAudit;

#[async_trait::async_trait]
impl AccountKeyAudit for TransparencyLogAudit {
    async fn audit(&self, state: &Arc<AppState>, user_id: &str) -> AuditStatus {
        match crate::commands::transparency::audit_peer_account_key(state, user_id.to_string()).await {
            Ok(report) => report.status,
            Err(e) => {
                crate::diag!("[bots] transparency audit of {user_id} failed to run: {e}");
                AuditStatus::Unavailable
            }
        }
    }
}

/// Test-harness only: a log that always answers the same, since the flows
/// harness runs no transparency log.
#[cfg(feature = "test-harness")]
pub struct FixedAudit(pub AuditStatus);

#[cfg(feature = "test-harness")]
#[async_trait::async_trait]
impl AccountKeyAudit for FixedAudit {
    async fn audit(&self, _state: &Arc<AppState>, _user_id: &str) -> AuditStatus {
        self.0
    }
}

/// Why a host refused an approval. Nothing is wrapped for any of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRefusal {
    /// Not a well-formed, canonical approval of a supported version.
    Malformed(String),
    /// The DS's copy of the request names a different request or approver.
    RequestMismatch,
    /// The signing device has no leaf in this host's view of the group.
    ApproverNotInGroup,
    /// The signing device is not certified by its account key, or is revoked.
    UncertifiedApprover(String),
    /// The signature does not verify under the signing device's certified key.
    BadSignature,
    /// The transparency log disagrees with the approver's account key.
    TransparencyAlarm,
    /// The approver is not a current admin of the group.
    NotAdmin(String),
    /// The approval is for another bot.
    WrongBot,
    /// The approval is for another group.
    WrongGroup,
    /// `expires_at` has passed.
    Expired,
    /// This host already acted on this request.
    Replayed,
    /// The request's ephemeral key on the DS is not the one the admin approved.
    Tampered,
}

impl std::fmt::Display for ApprovalRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed approval: {why}"),
            Self::RequestMismatch => write!(f, "the approval does not match the request the DS served"),
            Self::ApproverNotInGroup => write!(f, "the approving device is not in this host's view of the group"),
            Self::UncertifiedApprover(why) => write!(f, "the approving device is not certified: {why}"),
            Self::BadSignature => write!(f, "the approval's signature does not verify"),
            Self::TransparencyAlarm => write!(f, "the transparency log disagrees with the approver's account key"),
            Self::NotAdmin(why) => write!(f, "the approver is not a current admin of the group ({why})"),
            Self::WrongBot => write!(f, "the approval is for another bot"),
            Self::WrongGroup => write!(f, "the approval is for another group"),
            Self::Expired => write!(f, "the approval has expired"),
            Self::Replayed => write!(f, "this request was already acted on"),
            Self::Tampered => write!(
                f,
                "tampering: the request's ephemeral key on the DS is not the one the admin approved"
            ),
        }
    }
}

/// What a host did with one approved request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostApprovalOutcome {
    /// This host wrapped the account key to the new host.
    Wrapped { request_id: String, new_device_id: String },
    /// Verified, but another host wrapped first; recorded as consumed.
    WrappedByAnother { request_id: String },
    /// Verified, but a lower-ranked online host goes first (see [`WRAP_STAGGER_SECS`]).
    /// Call [`handle_host_approvals`] again at `retry_at` (unix seconds) if it is still pending.
    Deferred { request_id: String, retry_at: i64 },
    /// Refused; nothing wrapped.
    Refused { request_id: String, reason: ApprovalRefusal },
}

/// Verify one approved add-host request from this host's own state (#1182
/// "Adding a host" step 3). `Ok(Err(_))` is a refusal; `Err` a failure to check.
pub async fn verify_host_approval(
    state: &Arc<AppState>,
    session: &BotSession,
    pending: &PendingBotHostWire,
    audit: &dyn AccountKeyAudit,
    now: i64,
) -> Result<std::result::Result<HostApproval, ApprovalRefusal>> {
    use ApprovalRefusal as R;

    let (approval, payload, signature) = match approval::decode_unverified(&pending.host_approval) {
        Ok(v) => v,
        Err(ApprovalDecodeError::Malformed(why)) => return Ok(Err(R::Malformed(why.to_string()))),
        Err(ApprovalDecodeError::BadSignature) => return Ok(Err(R::BadSignature)),
    };
    if approval.v != HOST_APPROVAL_VERSION {
        return Ok(Err(R::Malformed(format!("unsupported version {}", approval.v))));
    }
    if approval.request_id != pending.request_id
        || approval.approver_user_id != pending.approver_user_id
        || approval.approver_device_id != pending.approver_device_id
    {
        return Ok(Err(R::RequestMismatch));
    }

    // The signer: a leaf in OUR view of the group, certified by its account key.
    let Some((leaf_key, scheme)) = crate::commands::mls::local_leaf_signature_key(
        state,
        &session.group_id,
        &approval.approver_user_id,
        &approval.approver_device_id,
    )
    .await
    else {
        return Ok(Err(R::ApproverNotInGroup));
    };
    if scheme != openmls::prelude::SignatureScheme::MLDSA44 {
        return Ok(Err(R::UncertifiedApprover(format!(
            "the group signs with {scheme:?}, not ML-DSA-44"
        ))));
    }
    let directory = crate::commands::mls::load_pinned_identities(
        state,
        std::slice::from_ref(&approval.approver_user_id),
        &session.bot_user_id,
    )
    .await?;
    match directory.leaf_verdict(&approval.approver_user_id, &approval.approver_device_id, &leaf_key, scheme) {
        crate::commands::mls::LeafVerdict::Certified => {}
        crate::commands::mls::LeafVerdict::Unverifiable(why) => return Ok(Err(R::UncertifiedApprover(why.to_string()))),
        crate::commands::mls::LeafVerdict::Uncertified(why) => return Ok(Err(R::UncertifiedApprover(why))),
    }
    if approval::verify_signature(&leaf_key, &payload, &signature).is_err() {
        return Ok(Err(R::BadSignature));
    }

    // What it approves.
    if approval.bot_user_id != session.bot_user_id {
        return Ok(Err(R::WrongBot));
    }
    if approval.group_id != session.group_id {
        return Ok(Err(R::WrongGroup));
    }
    if approval.expires_at <= now {
        return Ok(Err(R::Expired));
    }
    if approval.expires_at - approval.issued_at > HOST_APPROVAL_MAX_TTL_SECS || approval.issued_at > now + ISSUED_AT_SKEW_SECS {
        return Ok(Err(R::Malformed("lifetime out of bounds".to_string())));
    }
    if is_consumed(state, &approval.request_id).await? {
        return Ok(Err(R::Replayed));
    }
    let approved_key = b64().decode(&approval.new_host_ephemeral_pub).map_err(|_| err("approval key not base64"))?;
    let served_key = crate::commands::ds_reads::decode_b64("ephemeral_pub", &pending.ephemeral_pub)?;
    if !pollis_device_cert::constant_time_eq(&approved_key, &served_key) {
        crate::diag!(
            "[bots] TAMPERING: request {} on the DS carries an ephemeral key the admin did not approve",
            approval.request_id
        );
        return Ok(Err(R::Tampered));
    }

    if audit.audit(state, &approval.approver_user_id).await == AuditStatus::Alarm {
        return Ok(Err(R::TransparencyAlarm));
    }
    // Admin in the roster read too; the MLS view above said they are in the group.
    if let Some(why) = not_admin_in_roster(state, session, &approval.approver_user_id).await? {
        return Ok(Err(R::NotAdmin(why)));
    }
    Ok(Ok(approval))
}

/// `Some(reason)` unless the roster read lists `user_id` as an admin of the group.
async fn not_admin_in_roster(state: &Arc<AppState>, session: &BotSession, user_id: &str) -> Result<Option<String>> {
    let resp = crate::commands::ds_reads::group(state, &session.group_id, &session.bot_user_id, false).await?;
    if !resp.authorized {
        return Ok(Some("this host cannot read its group's roster".to_string()));
    }
    Ok(match resp.members.iter().find(|m| m.user_id == user_id) {
        Some(m) if m.role == "admin" => None,
        Some(_) => Some("roster: member, not admin".to_string()),
        None => Some("roster: not a member".to_string()),
    })
}

/// This host's rank among the bot's online hosts (lowest device id first), so
/// exactly one wraps first and the others take over in turn if it is gone.
async fn wrap_rank(state: &Arc<AppState>, session: &BotSession, now: i64) -> Result<usize> {
    let account_pub = {
        let guard = state.unlock.lock().await;
        let u = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
        account_pub_of(
            u.account_id_key
                .as_slice()
                .try_into()
                .map_err(|_| err("account key is not a 32-byte seed"))?,
        )
    };
    let rows = crate::commands::ds_reads::devices(state, Some(&session.bot_user_id), Vec::new(), false).await?;
    let mut online: Vec<String> = rows
        .iter()
        .filter(|r| r.device_id == session.device_id || is_recent(r.last_seen.as_deref(), now))
        .filter(|r| crate::commands::history_transfer::verify_device_row(&account_pub, r).is_ok())
        .map(|r| r.device_id.clone())
        .collect();
    if !online.contains(&session.device_id) {
        online.push(session.device_id.clone());
    }
    online.sort();
    Ok(online.iter().position(|d| *d == session.device_id).unwrap_or(0))
}

fn is_recent(last_seen: Option<&str>, now: i64) -> bool {
    last_seen
        .and_then(crate::commands::mls::parse_ds_instant)
        .is_some_and(|t| t.timestamp() >= now - ONLINE_WINDOW_SECS)
}

async fn is_consumed(state: &Arc<AppState>, request_id: &str) -> Result<bool> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
    Ok(db
        .conn()
        .query_row(
            "SELECT 1 FROM bot_consumed_host_request WHERE request_id = ?1",
            rusqlite::params![request_id],
            |_| Ok(()),
        )
        .is_ok())
}

async fn mark_consumed(state: &Arc<AppState>, request_id: &str) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
    db.conn().execute(
        "INSERT OR IGNORE INTO bot_consumed_host_request (request_id) VALUES (?1)",
        rusqlite::params![request_id],
    )?;
    Ok(())
}

/// Act on every approved add-host request for this bot: verify each from this
/// host's own state, and — if this host's turn has come — wrap the account key
/// to the new host through the enrollment wrap. Call on a `bot_host_approval`
/// inbox wake-up, on start, and at a `Deferred` outcome's `retry_at`.
pub async fn handle_host_approvals(
    state: &Arc<AppState>,
    session: &BotSession,
    audit: &dyn AccountKeyAudit,
) -> Result<Vec<HostApprovalOutcome>> {
    let pending = crate::commands::mls::ds_post_json(state, &PendingBotHostsBody {}).await?.requests;
    let mut outcomes = Vec::with_capacity(pending.len());
    for request in pending {
        outcomes.push(handle_one(state, session, &request, audit).await?);
    }
    Ok(outcomes)
}

async fn handle_one(
    state: &Arc<AppState>,
    session: &BotSession,
    pending: &PendingBotHostWire,
    audit: &dyn AccountKeyAudit,
) -> Result<HostApprovalOutcome> {
    let request_id = pending.request_id.clone();
    let now = chrono::Utc::now().timestamp();
    let approval = match verify_host_approval(state, session, pending, audit, now).await? {
        Ok(a) => a,
        Err(reason) => {
            crate::diag!("[bots] refused host approval for request {request_id}: {reason}");
            return Ok(HostApprovalOutcome::Refused { request_id, reason });
        }
    };
    let rank = wrap_rank(state, session, now).await?;
    let retry_at = approval.issued_at + rank as i64 * WRAP_STAGGER_SECS;
    if now < retry_at {
        return Ok(HostApprovalOutcome::Deferred { request_id, retry_at });
    }

    // Re-checked just before the wrap: an approver demoted since must not get one.
    if let Some(why) = not_admin_in_roster(state, session, &approval.approver_user_id).await? {
        return Ok(HostApprovalOutcome::Refused { request_id, reason: ApprovalRefusal::NotAdmin(why) });
    }
    let ephemeral = crate::commands::ds_reads::decode_b64("ephemeral_pub", &pending.ephemeral_pub)?;
    let wrapped = {
        let seed = {
            let guard = state.unlock.lock().await;
            let u = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
            Zeroizing::new(u.account_id_key.to_vec())
        };
        crate::commands::device_enrollment::wrap_account_seed_to(&ephemeral, &seed)?
    };
    let body = WrapBotHostBody {
        request_id: request_id.clone(),
        wrapped_account_key: b64().encode(&wrapped),
    };
    let resp = crate::commands::mls::ds_post(state, &body).await?;
    let status = resp.status();
    if status.is_success() {
        mark_consumed(state, &request_id).await?;
        crate::diag!("[bots] wrapped the account key for new host {} (request {request_id})", pending.device_id);
        return Ok(HostApprovalOutcome::Wrapped { request_id, new_device_id: pending.device_id.clone() });
    }
    if status == reqwest::StatusCode::CONFLICT {
        mark_consumed(state, &request_id).await?;
        return Ok(HostApprovalOutcome::WrappedByAnother { request_id });
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Ok(HostApprovalOutcome::Refused {
            request_id,
            reason: ApprovalRefusal::NotAdmin("the DS says the approver is no longer an admin".to_string()),
        });
    }
    let txt = resp.text().await.unwrap_or_default();
    Err(err(format!("bot host wrap ({status}): {txt}")))
}
