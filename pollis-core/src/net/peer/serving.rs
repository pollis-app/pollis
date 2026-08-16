//! The live relay-serving manager: consent in, a running (or not running) relay
//! node out, and a pushed status event whenever that answer changes.
//!
//! This is the engine behind `get_relay_serving_status` / `set_relay_serving`
//! (`crate::commands::relay_serving`), the same role
//! `commands::overlay::apply_overlay_mode` plays for the mode control.
//!
//! # One per process
//!
//! Relay serving is a property of the device, not of a session or an account: it
//! survives login, does not fan out per user, and there must never be two nodes
//! competing to serve. So the live manager is a process singleton ([`manager`]),
//! not a field on `AppState`. Tests build their own with
//! [`RelayServingManager::new`] rather than sharing the global.
//!
//! # Reading the status is not free of side effects, on purpose
//!
//! [`RelayServingManager::status`] re-probes the platform and reconciles the
//! engine, so what it returns is what the device is actually doing rather than
//! what it was doing when something last changed. That is also how an unplugged
//! charger gets noticed without a timer (CLAUDE.md bans polling): every status
//! read, every apply, and every link coming or going re-evaluates.
//!
//! # Why the engine runs while the status says `Waiting{NoInboundPath}`
//!
//! Reachability is the one condition the device earns by *trying*: the node has
//! to be up and attached to a first-party relay before anything can hand it a
//! circuit. So consent plus the user's own conditions (Wi-Fi, power) decide
//! whether the node runs, and reachability decides whether the status says
//! `Serving`. The user is told the truth either way — "paused: other devices
//! can't reach this one" is exactly that state.
//!
//! Since #813 wave 3 that state is no longer the steady state: starting the node
//! also starts [`super::reachability`], which parks an outbound connection at
//! every first-party relay in the signed directory and reports back the moment
//! one is live. `NoInboundPath` now means what it says — nothing can reach this
//! device *right now* — rather than "this feature is not finished".
//!
//! # The engine's revocation store
//!
//! [`PeerEngine::start`] takes a [`RevocationStore`] because a node that cannot
//! evaluate revocation refuses to extend, and extending is a peer's entire job.
//! The store built here is **directory-backed**: keyed on the same pinned
//! `POLLIS_OVERLAY_DIRECTORY_KEY` clients pin the directory with, and kept
//! current by the reachability loop that already reads that directory. A build
//! with no directory key gets [`RevocationStore::unconfigured`], which admits
//! nothing — so such a device runs a node, parks nowhere, and forwards nothing,
//! which is the honest composition rather than a fail-open shortcut.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use pollis_relay::policy::RevocationStore;

use crate::sink::EventSink;

use super::conditions::{
    hold_for, RelayServingConfig, RelayServingStatus, ServingSignals,
};
use super::context::ServingContext;
use super::engine::{PeerCounters, PeerEngine};
use super::link::PeerLink;
use super::park::LinkAcceptor;
use super::platform;
use super::reachability::{Reachability, ReachabilityConfig};

/// The global event name the renderer listens on. Must match
/// `RELAY_SERVING_EVENT` in `frontend/src/hooks/queries/useRelayServing.ts`.
pub const RELAY_SERVING_EVENT: &str = "relay-serving-status";

/// Whether this build can serve as a relay at all.
///
/// Mobile is excluded: a backgrounded app cannot hold a socket open on iOS, and
/// on both platforms the device is usually the metered, battery-powered one the
/// conditions exist to protect. `Unsupported` says that plainly instead of
/// leaving a mobile user staring at a toggle that never turns green.
const SUPPORTED: bool = !cfg!(any(target_os = "ios", target_os = "android"));

static MANAGER: OnceLock<Arc<RelayServingManager>> = OnceLock::new();

/// The process-wide manager.
pub fn manager() -> &'static Arc<RelayServingManager> {
    MANAGER.get_or_init(RelayServingManager::new)
}

/// Install the sink the `relay-serving-status` event is pushed through. The
/// desktop shell wires this to `AppHandle::emit` at setup; a build with no sink
/// simply pushes nothing and the UI falls back to reading on mount.
pub fn set_event_sink(sink: Arc<dyn EventSink<RelayServingStatus>>) {
    manager().set_sink(sink);
}

/// Give the process-wide manager the app-shaped inputs it needs to become
/// reachable (the signed directory and the device identity). Called once at
/// setup; until then the device runs its node but parks nowhere.
pub fn set_serving_context(context: Arc<dyn ServingContext>) {
    manager().set_context(context);
}

struct Inner {
    config: RelayServingConfig,
    engine: Option<PeerEngine>,
    /// Owns the parked connections. Dropped with the engine, which is what makes
    /// withdrawing consent stop reachability immediately.
    reachability: Option<Reachability>,
}

pub struct RelayServingManager {
    /// Async because reconciling starts and drains a node.
    inner: tokio::sync::Mutex<Inner>,
    sink: Mutex<Option<Arc<dyn EventSink<RelayServingStatus>>>>,
    /// The last status pushed, so an unchanged reconcile pushes nothing.
    last: Mutex<Option<RelayServingStatus>>,
    counters: Arc<PeerCounters>,
    /// True once something can hand this device circuits. Set by
    /// [`super::reachability`] when a parked connection goes live or is lost;
    /// **false** until then, which is why a consenting device with no directory
    /// (or a down pool) settles on `Waiting{NoInboundPath}` rather than claiming
    /// to be serving.
    inbound_path: AtomicBool,
    /// Installed at setup by the shell. `None` in a build that never installs
    /// one — that device runs its node and is simply not reachable.
    context: Mutex<Option<Arc<dyn ServingContext>>>,
    /// A handle back to the `Arc` this manager lives in, so a reconcile can hand
    /// a `Weak` to the tasks it spawns without threading one through every call.
    me: OnceLock<Weak<RelayServingManager>>,
}

impl RelayServingManager {
    /// A fresh manager, with the counter change-hook already wired so link
    /// churn pushes a status event.
    pub fn new() -> Arc<RelayServingManager> {
        let manager = Arc::new(RelayServingManager {
            inner: tokio::sync::Mutex::new(Inner {
                config: RelayServingConfig::default(),
                engine: None,
                reachability: None,
            }),
            sink: Mutex::new(None),
            last: Mutex::new(None),
            counters: PeerCounters::new(),
            inbound_path: AtomicBool::new(false),
            context: Mutex::new(None),
            me: OnceLock::new(),
        });
        let _ = manager.me.set(Arc::downgrade(&manager));

        // Weak so the hook never keeps the manager alive, and so a dropped
        // test-local manager's late-firing hook is a no-op rather than a panic.
        let weak = Arc::downgrade(&manager);
        manager.counters.set_on_change(Arc::new(move || {
            let Some(manager) = weak.upgrade() else {
                return;
            };
            // The hook fires from a pump task (and from `Drop`), so it must not
            // block: hand the reconcile to the runtime, if there is one.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    manager.refresh().await;
                });
            }
        }));
        manager
    }

    /// The CURRENT live status. Re-probes and reconciles first — see the module
    /// docs for why a read does work.
    pub async fn status(&self) -> RelayServingStatus {
        let config = self.inner.lock().await.config;
        self.reconcile(config).await
    }

    /// Apply `config` live and return the status it settled on. Idempotent, so
    /// the UI can call it on every toggle and once after login/restart.
    pub async fn apply(&self, config: RelayServingConfig) -> RelayServingStatus {
        self.reconcile(config).await
    }

    /// Reconcile and push an event if anything changed. Used by the counter
    /// hook; callers that want the value use [`RelayServingManager::status`].
    pub async fn refresh(&self) {
        let _ = self.status().await;
    }

    /// Record whether anything can currently hand this device circuits. Called
    /// by the transport that attaches the device to a first-party relay.
    pub fn set_inbound_path(&self, present: bool) {
        self.inbound_path.store(present, Ordering::Relaxed);
    }

    /// Hand a link to the running node. Returns false when nothing is running —
    /// a link offered to a device that is off or on hold is refused, not queued.
    pub async fn attach_link(&self, link: PeerLink) -> bool {
        let inner = self.inner.lock().await;
        match &inner.engine {
            Some(engine) => {
                engine.serve_link(link);
                true
            }
            None => false,
        }
    }

    pub fn counters(&self) -> &Arc<PeerCounters> {
        &self.counters
    }

    fn set_sink(&self, sink: Arc<dyn EventSink<RelayServingStatus>>) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    /// Install the app-shaped inputs reachability needs. Replaces any previous
    /// one; a context installed after the node started takes effect at the next
    /// reconcile, which the caller triggers with an apply or a status read.
    fn set_context(&self, context: Arc<dyn ServingContext>) {
        *self.context.lock().unwrap() = Some(context);
    }

    fn context(&self) -> Option<Arc<dyn ServingContext>> {
        self.context.lock().unwrap().clone()
    }

    /// The engine's revocation store, keyed on the same pinned directory key the
    /// client pins the directory with.
    ///
    /// [`RevocationStore::unconfigured`] when there is no key. That admits
    /// nothing, so such a device forwards nothing — deliberately, and not as an
    /// oversight: a peer's only job is to extend to a next hop, and extending
    /// without being able to check whether that hop has been revoked is the
    /// fail-open this whole phase exists to prevent.
    fn revocation_store(&self) -> RevocationStore {
        match self.context().and_then(|c| c.directory()) {
            Some((_, key)) => RevocationStore::enforcing(key),
            None => RevocationStore::unconfigured(),
        }
    }

    /// Start parking outbound connections for the running engine.
    ///
    /// Returns `None` when there is no context to park with, in which case the
    /// device stays on `Waiting{NoInboundPath}` — the truthful answer.
    fn start_reachability(
        &self,
        leaf_der: pollis_relay::CertificateDer<'static>,
        revocations: RevocationStore,
    ) -> Option<Reachability> {
        let context = self.context()?;
        let me = self.me.get()?.clone();

        // Reachability changed ⇒ push a status event. Event-driven, so an
        // unplugged relay is noticed without anything polling for it.
        let on_change: Arc<dyn Fn(usize) + Send + Sync> = {
            let me = me.clone();
            Arc::new(move |live: usize| {
                let Some(manager) = me.upgrade() else {
                    return;
                };
                manager.set_inbound_path(live > 0);
                // The hook fires from a park task (and from `Drop`), so it must
                // not block: hand the reconcile to the runtime, if there is one.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        manager.refresh().await;
                    });
                }
            })
        };

        Some(Reachability::start(ReachabilityConfig {
            context,
            revocations,
            leaf_der,
            acceptor: Arc::new(ManagerAcceptor(me)),
            on_change,
        }))
    }

    /// The whole state machine in one place: probe, decide whether the node
    /// should be up, make that true, then report what is actually the case.
    async fn reconcile(&self, config: RelayServingConfig) -> RelayServingStatus {
        let link = platform::probe().await;
        // An engine taken out for shutdown, drained after the lock is released.
        let mut draining = None;
        let mut inner = self.inner.lock().await;
        inner.config = config;

        // Consent + the user's own conditions decide whether the node runs;
        // reachability does not (see the module docs), so it is held true here.
        let engine_wanted = SUPPORTED
            && config.enabled
            && hold_for(
                config,
                ServingSignals {
                    link,
                    inbound_path: true,
                },
            )
            .is_none();

        if engine_wanted && inner.engine.is_none() {
            // The store is directory-backed and shared with the reachability
            // loop that keeps it current (see the module docs). It starts empty,
            // which means the node refuses to extend until it holds a verified,
            // unexpired list — fail-closed from the first instant, not from the
            // first refresh.
            let revocations = self.revocation_store();
            match PeerEngine::start(self.counters.clone(), revocations.clone()) {
                Ok(engine) => {
                    // Parking carries the engine's leaf: that cert IS this
                    // peer's identity, and it is what a client pins when the
                    // directory offers this device as a hop.
                    inner.reachability =
                        self.start_reachability(engine.cert_der().clone(), revocations);
                    inner.engine = Some(engine);
                }
                Err(e) => {
                    // Failing to stand the node up is not a reason to claim we
                    // are serving. Leave it down; the status below reports the
                    // truth and the UI says we cannot confirm what is happening.
                    eprintln!("[peer-relay] could not start the peer node: {e}");
                }
            }
        } else if !engine_wanted && inner.engine.is_some() {
            self.inbound_path.store(false, Ordering::Relaxed);
            // Dropping this closes every parked connection, so the device stops
            // being reachable at the same moment it stops being willing.
            inner.reachability = None;
            // Take it out here, drain it below — see `draining`.
            draining = inner.engine.take();
        }

        let running = inner.engine.is_some();
        drop(inner);

        // The drain happens OUTSIDE the manager mutex. `PeerEngine::shutdown`
        // deliberately waits for someone else's in-flight message to finish, and
        // `attach_link` — the path EVERY inbound circuit takes, driven by the
        // parking supervisor — wants this same mutex. Draining under the lock
        // therefore blocked every new inbound link for the whole wait, for no
        // reason: the engine has already been moved out, so nothing else can
        // reach it. The observable order is unchanged (drain, then evaluate,
        // then emit) so the reported link count still reflects the post-drain
        // state.
        if let Some(mut engine) = draining {
            engine.shutdown().await;
        }

        let signals = ServingSignals {
            link,
            inbound_path: running && self.inbound_path.load(Ordering::Relaxed),
        };
        let status = RelayServingStatus::evaluate(
            config,
            signals,
            SUPPORTED,
            self.counters.active_links(),
            self.counters.bytes_forwarded(),
        );

        self.emit_if_changed(status);
        status
    }

    /// Push the status when it differs from the last one pushed. Bytes forwarded
    /// changes constantly, so it is excluded from the comparison — the UI does
    /// not need a per-datagram event, and CLAUDE.md's no-polling rule cuts both
    /// ways: no timer, and no event storm either.
    fn emit_if_changed(&self, status: RelayServingStatus) {
        let mut last = self.last.lock().unwrap();
        let changed = match *last {
            Some(previous) => {
                previous.state != status.state
                    || previous.hold != status.hold
                    || previous.config != status.config
                    || previous.on_wifi != status.on_wifi
                    || previous.on_power != status.on_power
                    || previous.active_circuits != status.active_circuits
            }
            None => true,
        };
        *last = Some(status);
        drop(last);

        if !changed {
            return;
        }
        let sink = self.sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            if let Err(e) = sink.send(status) {
                eprintln!("[peer-relay] status event dropped: {e}");
            }
        }
    }
}

/// Hands a spliced link to whatever node the manager is currently running.
///
/// `Weak`, so an aborted-but-still-draining park task cannot keep a manager (and
/// with it a relay node) alive after consent is withdrawn.
struct ManagerAcceptor(Weak<RelayServingManager>);

#[async_trait::async_trait]
impl LinkAcceptor for ManagerAcceptor {
    async fn accept(&self, link: PeerLink) -> bool {
        match self.0.upgrade() {
            Some(manager) => manager.attach_link(link).await,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::peer::conditions::{RelayServingHold, RelayServingState};
    use std::sync::Mutex as StdMutex;

    /// Captures pushed events.
    struct RecordingSink(Arc<StdMutex<Vec<RelayServingStatus>>>);

    impl EventSink<RelayServingStatus> for RecordingSink {
        fn send(&self, event: RelayServingStatus) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    /// A context that reports a directory but can never produce an identity —
    /// enough to exercise the store wiring without standing up a signed pool.
    struct FakeContext(Option<(String, String)>);

    #[async_trait::async_trait]
    impl ServingContext for FakeContext {
        fn directory(&self) -> Option<(String, String)> {
            self.0.clone()
        }

        async fn identity(&self) -> anyhow::Result<Arc<pollis_relay::client::ClientIdentity>> {
            anyhow::bail!("no identity in this test")
        }
    }

    fn consent_without_conditions() -> RelayServingConfig {
        RelayServingConfig {
            enabled: true,
            wifi_only: false,
            power_only: false,
        }
    }

    #[tokio::test]
    async fn a_fresh_manager_is_off() {
        let manager = RelayServingManager::new();
        let status = manager.status().await;
        assert_eq!(status.state, RelayServingState::Off);
        assert_eq!(status.hold, None);
        assert_eq!(status.config, RelayServingConfig::default());
        assert!(!status.config.enabled);
    }

    /// Consent alone never reports `Serving`: with no way to reach this device,
    /// the honest answer is the reachability hold.
    #[tokio::test]
    async fn consent_settles_to_waiting_without_an_inbound_path() {
        let manager = RelayServingManager::new();
        let status = manager.apply(consent_without_conditions()).await;
        assert_eq!(status.state, RelayServingState::Waiting);
        assert_eq!(status.hold, Some(RelayServingHold::NoInboundPath));
        assert!(status.config.enabled);
    }

    /// The node runs while the status is `Waiting{NoInboundPath}` — that is how
    /// it gets an inbound path at all — and once one exists the state flips
    /// without any config change.
    #[tokio::test]
    async fn an_inbound_path_flips_waiting_to_serving() {
        let manager = RelayServingManager::new();
        assert_eq!(
            manager.apply(consent_without_conditions()).await.hold,
            Some(RelayServingHold::NoInboundPath)
        );

        manager.set_inbound_path(true);
        let status = manager.status().await;
        assert_eq!(status.state, RelayServingState::Serving);
        assert_eq!(status.hold, None);
    }

    /// A link is refused while nothing is running, and accepted once it is.
    #[tokio::test]
    async fn links_are_only_accepted_while_the_node_runs() {
        let manager = RelayServingManager::new();
        let (_client, peer_end) = super::super::link::loopback_pair();
        assert!(!manager.attach_link(peer_end).await);

        manager.apply(consent_without_conditions()).await;
        let (_client2, peer_end2) = super::super::link::loopback_pair();
        assert!(manager.attach_link(peer_end2).await);
    }

    #[tokio::test]
    async fn applying_is_idempotent() {
        let manager = RelayServingManager::new();
        let first = manager.apply(consent_without_conditions()).await;
        let second = manager.apply(consent_without_conditions()).await;
        assert_eq!(first.state, second.state);
        assert_eq!(first.hold, second.hold);
        assert_eq!(first.config, second.config);
    }

    #[tokio::test]
    async fn withdrawing_consent_stops_the_node_and_clears_reachability() {
        let manager = RelayServingManager::new();
        manager.apply(consent_without_conditions()).await;
        manager.set_inbound_path(true);
        assert_eq!(manager.status().await.state, RelayServingState::Serving);

        let off = manager.apply(RelayServingConfig::default()).await;
        assert_eq!(off.state, RelayServingState::Off);
        assert_eq!(off.hold, None);
        // A link offered after consent is withdrawn goes nowhere.
        let (_client, peer_end) = super::super::link::loopback_pair();
        assert!(!manager.attach_link(peer_end).await);
        // And reachability does not survive the node it belonged to.
        assert!(!manager.inbound_path.load(Ordering::Relaxed));
    }

    /// **Contract §5.** The engine is handed the client's *directory-backed*
    /// store, keyed on the same pinned key the directory is verified with — so a
    /// serving peer can actually extend once it holds a list.
    #[tokio::test]
    async fn a_directory_backed_context_gives_the_engine_an_enforcing_store() {
        let manager = RelayServingManager::new();
        manager.set_context(Arc::new(FakeContext(Some((
            "https://relays.pollis.com/v1/directory.json".to_string(),
            "aGVsbG8gd29ybGQgdGhpcyBpcyAzMiBieXRlcyEh".to_string(),
        )))));
        assert!(
            manager.revocation_store().is_configured(),
            "a peer with a directory must be able to evaluate revocation"
        );
    }

    /// And the other half of §5: no directory ⇒ no key ⇒ the store admits
    /// nothing, so the device runs a node and forwards nothing. Being unable to
    /// evaluate revocation is a refusal, never a default-open.
    #[tokio::test]
    async fn without_a_directory_the_engine_cannot_evaluate_revocation() {
        let manager = RelayServingManager::new();
        assert!(!manager.revocation_store().is_configured());

        manager.set_context(Arc::new(FakeContext(None)));
        assert!(!manager.revocation_store().is_configured());
    }

    /// A device with no way to be reached settles on the reachability hold and
    /// stays there — the honest answer, and the one the UI renders.
    #[tokio::test]
    async fn consent_without_a_directory_settles_on_no_inbound_path() {
        let manager = RelayServingManager::new();
        manager.set_context(Arc::new(FakeContext(None)));
        let status = manager.apply(consent_without_conditions()).await;
        assert_eq!(status.state, RelayServingState::Waiting);
        assert_eq!(status.hold, Some(RelayServingHold::NoInboundPath));
    }

    #[tokio::test]
    async fn the_event_fires_on_change_and_stays_quiet_otherwise() {
        let manager = RelayServingManager::new();
        let events = Arc::new(StdMutex::new(Vec::new()));
        manager.set_sink(Arc::new(RecordingSink(events.clone())));

        // First reconcile always pushes (nothing has been reported yet).
        manager.status().await;
        assert_eq!(events.lock().unwrap().len(), 1);

        // Same state again: no push.
        manager.status().await;
        assert_eq!(events.lock().unwrap().len(), 1);

        // A real change pushes.
        manager.apply(consent_without_conditions()).await;
        let pushed = events.lock().unwrap().clone();
        assert_eq!(pushed.len(), 2);
        assert_eq!(pushed[1].state, RelayServingState::Waiting);
        assert_eq!(pushed[1].hold, Some(RelayServingHold::NoInboundPath));
    }
}
