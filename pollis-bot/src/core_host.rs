//! The real [`BotHost`] and [`InstallationCore`]: one `pollis-core` `AppState`
//! per installation, each with its own data dir (`AppState::local_dir`) and
//! keystore slot, so installations in one process share nothing.
//!
//! A thin adapter: pairing, unattended start and group join are
//! `pollis_core::commands::bots`; catch-up is the normal ingest path; posting is
//! the normal durable send path; wake-ups are the normal realtime rooms (the
//! group's room and the bot's inbox), behind the `realtime` feature.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pollis_core::commands::bots::{self, BotSession, PairingOutcome};
use pollis_core::commands::groups::list_group_channels;
use pollis_core::commands::messages::{get_channel_messages, read_messages_after, send_message_with_id, MessageCursor};
use pollis_core::config::Config;
use pollis_core::keystore::Keystore;
use pollis_core::state::AppState;

use crate::action::ChannelInfo;
use crate::core::{CoreError, InstallationCore};
use crate::host::{BotHost, OpenedInstallation, PairOutcome, PairTarget, PairingProgress, PairingTicket, WakeFn};
use crate::installations::InstallationRecord;
use crate::secrets::SecretSlot;
use crate::trigger::{IncomingMessage, TriggerKey};

/// Each installation's encrypted store lives in `<data dir>/stores/<device id>/`.
/// Keyed by the host's device id because pairing opens the store before the
/// bot's id is known, and the device id is fixed when the request is filed.
pub const STORES_DIR: &str = "stores";
/// How long a fresh host waits for the admin's Welcome before joining directly.
const JOIN_WAIT: Duration = Duration::from_secs(60);
/// On restart the group is already local; this only drains queued Welcomes.
const REJOIN_WAIT: Duration = Duration::from_secs(5);

pub struct CoreHost {
    config: Config,
    keystore: Arc<dyn Keystore>,
}

impl CoreHost {
    /// A host over `data_dir`. Its keystore entries are namespaced by the data
    /// dir, so two runtimes on one machine (say, two hosts of one bot under one
    /// user) never share a slot.
    pub fn new(data_dir: &Path) -> Result<Self> {
        let config = Config::from_env().context(
            "reading the Delivery Service settings from the environment. Fix: set POLLIS_DELIVERY_URL (and the other variables the `pollis` CLI uses)",
        )?;
        let keystore: Arc<dyn Keystore> = Arc::new(ScopedKeystore::new(
            pollis_core::keystore::default_os_keystore(),
            data_dir,
        ));
        Ok(Self { config, keystore })
    }

    fn state_for(&self, store_dir: PathBuf) -> Result<Arc<AppState>> {
        std::fs::create_dir_all(&store_dir).with_context(|| format!("creating {}", store_dir.display()))?;
        let state = AppState::new_with_parts(self.config.clone(), self.keystore.clone());
        state
            .local_dir
            .set(store_dir)
            .map_err(|_| anyhow!("an installation's data dir was set twice"))?;
        Ok(Arc::new(state))
    }
}

/// The OS keystore, with every entry this runtime writes prefixed by a hash of
/// its data dir. A person's `pin_meta` slot is also read unprefixed, so the
/// core's refusal of a human account on the unattended path still sees it.
pub struct ScopedKeystore {
    inner: Arc<dyn Keystore>,
    prefix: String,
}

impl ScopedKeystore {
    pub fn new(inner: Arc<dyn Keystore>, data_dir: &Path) -> Self {
        use sha2::{Digest, Sha256};
        let canonical = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
        let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
        Self { inner, prefix: format!("pollis_bot_{}_", hex::encode(&digest[..8])) }
    }

    fn scoped(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

#[async_trait]
impl Keystore for ScopedKeystore {
    async fn store(&self, key: &str, value: &[u8]) -> pollis_core::error::Result<()> {
        Keystore::store(&*self.inner, &self.scoped(key), value).await
    }

    async fn load(&self, key: &str) -> pollis_core::error::Result<Option<Vec<u8>>> {
        if let Some(v) = Keystore::load(&*self.inner, &self.scoped(key)).await? {
            return Ok(Some(v));
        }
        if key.starts_with("pin_meta") {
            return Keystore::load(&*self.inner, key).await;
        }
        Ok(None)
    }

    async fn delete(&self, key: &str) -> pollis_core::error::Result<()> {
        Keystore::delete(&*self.inner, &self.scoped(key)).await
    }
}

fn store_dir(data_dir: &Path, device_id: &str) -> Result<PathBuf> {
    if !crate::installations::valid_id(device_id) {
        return Err(anyhow!("`{device_id}` is not a device id"));
    }
    Ok(data_dir.join(STORES_DIR).join(device_id))
}

/// `dir` is `<data dir>/installations/<id>`; the data dir is two levels up.
fn data_dir_of(installation_dir: &Path) -> Result<&Path> {
    installation_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow!("{} is not inside a data dir", installation_dir.display()))
}

#[async_trait]
impl BotHost for CoreHost {
    async fn pair(
        &self,
        data_dir: &Path,
        target: PairTarget,
        device_name: Option<String>,
        progress: &(dyn Fn(PairingProgress) + Send + Sync),
    ) -> Result<PairOutcome> {
        // The request goes out before any store exists; the store dir is named
        // by the device id the request carries.
        let staging = AppState::new_with_parts(self.config.clone(), self.keystore.clone());
        let staging = Arc::new(staging);
        let handle = match &target {
            PairTarget::NewBot => bots::start_new_bot_pairing(&staging, device_name).await?,
            PairTarget::ExistingBot { bot_user_id } => {
                bots::start_add_host_pairing(&staging, bot_user_id, device_name).await?
            }
        };
        let dir = store_dir(data_dir, &handle.device_id)?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        staging
            .local_dir
            .set(dir)
            .map_err(|_| anyhow!("the pairing state's data dir was set twice"))?;
        progress(PairingProgress::WaitingForApproval(PairingTicket {
            code: handle.code.clone(),
            expires_at: handle.expires_at,
        }));
        let session = match bots::await_pairing(&staging, &handle).await? {
            PairingOutcome::Expired => return Ok(PairOutcome::Expired),
            PairingOutcome::Paired(session) => session,
        };
        progress(PairingProgress::JoiningGroup);
        bots::join_bot_group(&staging, &session, JOIN_WAIT).await?;
        Ok(PairOutcome::Paired(InstallationRecord {
            installation_id: session.bot_user_id.clone(),
            bot_user_id: session.bot_user_id,
            group_id: session.group_id,
            device_id: session.device_id,
            handler: Default::default(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }))
    }

    async fn open(&self, record: &InstallationRecord, dir: &Path, wake: WakeFn) -> Result<OpenedInstallation> {
        let store = store_dir(data_dir_of(dir)?, &record.device_id)?;
        let state = self.state_for(store.clone())?;
        let session = bots::start_unattended(&state, &record.bot_user_id).await?;
        if session.group_id != record.group_id || session.device_id != record.device_id {
            return Err(anyhow!(
                "installation.json disagrees with the keystore about this bot's group or device. Fix: run `pollis-bot forget --installation {}` and pair again",
                record.installation_id
            ));
        }
        bots::join_bot_group(&state, &session, REJOIN_WAIT).await?;
        // Approvals that arrived while this host was down.
        handle_host_approvals(state.clone(), session.clone()).await;
        #[cfg(feature = "realtime")]
        realtime::connect(&state, &session, wake).await?;
        #[cfg(not(feature = "realtime"))]
        drop(wake);
        let core = installation_core(state, session);
        Ok(OpenedInstallation {
            core,
            secrets: Arc::new(self.keystore.clone()),
        })
    }

    async fn forget(&self, record: &InstallationRecord, dir: &Path) -> Result<()> {
        let store = store_dir(data_dir_of(dir)?, &record.device_id)?;
        let state = self.state_for(store.clone())?;
        bots::forget_installation(&state, &record.bot_user_id).await?;
        self.keystore
            .delete_for_user(crate::secrets::WEBHOOK_SECRET_KEY, &record.installation_id)
            .await?;
        drop(state);
        match std::fs::remove_dir_all(&store) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", store.display())),
        }
    }

    async fn secrets(&self, _record: &InstallationRecord, _dir: &Path) -> Result<Arc<dyn SecretSlot>> {
        Ok(Arc::new(self.keystore.clone()))
    }
}

/// Verify and wrap any approved add-host requests for this bot. Failures are
/// logged: they never stop the bot's own work. A `Deferred` outcome (another
/// host ranks first to wrap) schedules exactly one re-check at its `retry_at`;
/// approvals expire within minutes, so the chain ends.
fn handle_host_approvals(
    state: Arc<AppState>,
    session: BotSession,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let scope = crate::log::scope(&session.bot_user_id);
        match bots::handle_host_approvals(&state, &session, &bots::TransparencyLogAudit).await {
            Ok(outcomes) => {
                let mut retry_at: Option<i64> = None;
                for o in outcomes {
                    if let bots::HostApprovalOutcome::Deferred { retry_at: at, .. } = &o {
                        retry_at = Some(retry_at.map_or(*at, |r| r.min(*at)));
                    }
                    crate::log::info(&scope, format!("host approval: {o:?}"));
                }
                if let Some(at) = retry_at {
                    let wait = (at - chrono::Utc::now().timestamp()).max(1) as u64;
                    let (state, session) = (state.clone(), session.clone());
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                        handle_host_approvals(state, session).await;
                    });
                }
            }
            Err(e) => crate::log::failure(
                &scope,
                format!("checking host approvals: {e}"),
                "check this host can reach the Delivery Service; approvals are checked again on the next start or approval",
            ),
        }
    })
}

/// The job loop's view of one opened, unattended installation: `state` is
/// signed in as the bot (after `start_unattended` or pairing) and has joined its
/// group. Read cursors live in its encrypted store (`bot_trigger_cursor`).
/// Public so the flows harness can run the real loop over its in-process DS.
pub fn installation_core(state: Arc<AppState>, session: BotSession) -> Arc<dyn InstallationCore> {
    Arc::new(CoreInstallation { state, session })
}

struct CoreInstallation {
    state: Arc<AppState>,
    session: BotSession,
}

impl CoreInstallation {
    async fn text_channels(&self) -> Result<Vec<String>, CoreError> {
        Ok(self
            .channels()
            .await?
            .into_iter()
            .filter(|c| c.is_text)
            .map(|c| c.id)
            .collect())
    }
}

fn core_err(e: impl std::fmt::Display) -> CoreError {
    CoreError(e.to_string())
}

#[async_trait]
impl InstallationCore for CoreInstallation {
    fn installation_id(&self) -> &str {
        &self.session.bot_user_id
    }

    fn bot_user_id(&self) -> &str {
        &self.session.bot_user_id
    }

    async fn catch_up(&self) -> Result<Vec<IncomingMessage>, CoreError> {
        let user = self.session.bot_user_id.clone();
        pollis_core::commands::mls::catch_up_all_mls_groups(&self.state, &user)
            .await
            .map_err(core_err)?;
        let mut fresh = Vec::new();
        for channel_id in self.text_channels().await? {
            // Ingest + decrypt through the normal read path; the newest page seeds a new channel's cursor.
            let newest = get_channel_messages(user.clone(), channel_id.clone(), Some(1), None, &self.state)
                .await
                .map_err(core_err)?;
            let stored = bots::trigger_cursor(&self.state, &channel_id).await.map_err(core_err)?;
            let Some((sent_at, id)) = stored else {
                // First sight of this channel: start from now, never replay history.
                let (sent_at, id) = newest
                    .messages
                    .first()
                    .map(|m| (m.sent_at.clone(), m.id.clone()))
                    .unwrap_or_default();
                bots::set_trigger_cursor(&self.state, &channel_id, &sent_at, &id)
                    .await
                    .map_err(core_err)?;
                continue;
            };
            let mut after = MessageCursor { sent_at, id };
            loop {
                let page = read_messages_after(
                    channel_id.clone(),
                    MessageCursor {
                        sent_at: after.sent_at.clone(),
                        id: after.id.clone(),
                    },
                    Some(200),
                    &self.state,
                )
                .await
                .map_err(core_err)?;
                // Pages are newest-first.
                for m in page.messages.iter().rev() {
                    if m.deleted_at.is_some() {
                        continue;
                    }
                    fresh.push(IncomingMessage {
                        envelope_id: m.id.clone(),
                        channel_id: channel_id.clone(),
                        sender_id: m.sender_id.clone(),
                        text: if m.undecryptable { None } else { m.content.clone() },
                        reply_to_id: m.reply_to_id.clone(),
                        thread_id: m.thread_id.clone(),
                        sent_at: m.sent_at.clone(),
                    });
                }
                match page.next_cursor {
                    Some(next) => after = next,
                    None => break,
                }
            }
        }
        fresh.sort_by(|a, b| {
            (a.sent_at.as_str(), a.envelope_id.as_str()).cmp(&(b.sent_at.as_str(), b.envelope_id.as_str()))
        });
        Ok(fresh)
    }

    async fn mark_handled(&self, message: &IncomingMessage) -> Result<(), CoreError> {
        bots::set_trigger_cursor(&self.state, &message.channel_id, &message.sent_at, &message.envelope_id)
            .await
            .map_err(core_err)
    }

    async fn channels(&self) -> Result<Vec<ChannelInfo>, CoreError> {
        let channels = list_group_channels(
            self.session.group_id.clone(),
            self.session.bot_user_id.clone(),
            &self.state,
        )
        .await
        .map_err(core_err)?;
        Ok(channels
            .into_iter()
            .map(|c| ChannelInfo {
                is_text: c.channel_type == "text",
                id: c.id,
                name: c.name,
            })
            .collect())
    }

    async fn claim(&self, key: &TriggerKey) -> Result<bool, CoreError> {
        bots::claim_trigger(&self.state, &self.session.bot_user_id, key.as_str())
            .await
            .map_err(core_err)
    }

    async fn post(&self, channel_id: &str, text: &str, message_id: &str) -> Result<(), CoreError> {
        send_message_with_id(
            Some(message_id.to_string()),
            channel_id.to_string(),
            self.session.bot_user_id.clone(),
            text.to_string(),
            Vec::new(),
            None,
            None,
            None,
            &self.state,
        )
        .await
        .map(|_| ())
        .map_err(core_err)
    }
}

#[cfg(feature = "realtime")]
mod realtime {
    use std::sync::Arc;

    use anyhow::Result;
    use pollis_core::commands::bots::BotSession;
    use pollis_core::commands::livekit::{connect_rooms, subscribe_realtime};
    use pollis_core::realtime::RealtimeEvent;
    use pollis_core::sink::EventSink;
    use pollis_core::state::AppState;

    use crate::host::WakeFn;

    /// Turns the installation's realtime events into job-loop wake-ups. A
    /// wake-up is only a hint: the job's catch-up decides what is new.
    struct WakeSink {
        state: Arc<AppState>,
        session: BotSession,
        wake: WakeFn,
    }

    impl EventSink<RealtimeEvent> for WakeSink {
        fn send(&self, event: RealtimeEvent) -> Result<(), String> {
            match event {
                RealtimeEvent::NewMessage { .. }
                | RealtimeEvent::EditedMessage { .. }
                | RealtimeEvent::DeletedMessage { .. }
                | RealtimeEvent::MembershipChanged { .. }
                | RealtimeEvent::RosterChanged { .. } => (self.wake)(),
                RealtimeEvent::BotHostApproval { .. } => {
                    let (state, session) = (self.state.clone(), self.session.clone());
                    tokio::spawn(super::handle_host_approvals(state, session));
                }
                RealtimeEvent::RealtimeReconnected { .. } => {
                    // Missed events are not replayed: catch up, and look for approvals.
                    (self.wake)();
                    let (state, session) = (self.state.clone(), self.session.clone());
                    tokio::spawn(super::handle_host_approvals(state, session));
                }
                _ => {}
            }
            Ok(())
        }
    }

    pub(super) async fn connect(state: &Arc<AppState>, session: &BotSession, wake: WakeFn) -> Result<()> {
        let sink = Arc::new(WakeSink {
            state: state.clone(),
            session: session.clone(),
            wake,
        });
        subscribe_realtime(sink, state).await?;
        let rooms = vec![session.group_id.clone(), format!("inbox-{}", session.bot_user_id)];
        connect_rooms(rooms, session.bot_user_id.clone(), session.bot_user_id.clone(), state).await?;
        Ok(())
    }
}
