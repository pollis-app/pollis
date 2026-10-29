//! The live relay-serving manager: consent in, a running (or not running) relay
//! node out, and a pushed status event whenever that answer changes.
//!
//! Behind `get_relay_serving_status` / `set_relay_serving` (`crate::commands::relay_serving`).
//!
//! # One per process
//!
//! A device property, so a process singleton ([`manager`]); tests use [`RelayServingManager::new`].
//!
//! # Reading the status is not free of side effects, on purpose
//!
//! [`RelayServingManager::status`] re-probes and reconciles, which is how a charger change is noticed without a timer.
//!
//! # Why the engine runs while the status says `Waiting{NoInboundPath}`
//!
//! The node must run to become reachable; consent and conditions decide whether it runs, reachability decides `Serving`.
//!
//! # The engine's revocation store
//!
//! [`PeerEngine::start`] gets a directory-backed [`RevocationStore`]; without a key, [`RevocationStore::unconfigured`] admits nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use pollis_relay::nexthop::NextHopDirectory;
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

/// Must match `RELAY_SERVING_EVENT` in `useRelayServing.ts`.
pub const RELAY_SERVING_EVENT: &str = "relay-serving-status";

/// Whether this build can serve as a relay at all.
///
/// Not mobile: iOS can't hold a background socket, and phones are the metered, battery devices.
const SUPPORTED: bool = !cfg!(any(target_os = "ios", target_os = "android"));

static MANAGER: OnceLock<Arc<RelayServingManager>> = OnceLock::new();

/// The process-wide manager.
pub fn manager() -> &'static Arc<RelayServingManager> {
    MANAGER.get_or_init(RelayServingManager::new)
}

/// Sink for the `relay-serving-status` event; without one the UI reads on mount.
pub fn set_event_sink(sink: Arc<dyn EventSink<RelayServingStatus>>) {
    manager().set_sink(sink);
}

/// Install the directory + identity context. Until then the node parks nowhere.
pub fn set_serving_context(context: Arc<dyn ServingContext>) {
    manager().set_context(context);
}

struct Inner {
    config: RelayServingConfig,
    engine: Option<PeerEngine>,
    /// Parked connections; dropped with the engine.
    reachability: Option<Reachability>,
}

pub struct RelayServingManager {
    inner: tokio::sync::Mutex<Inner>,
    sink: Mutex<Option<Arc<dyn EventSink<RelayServingStatus>>>>,
    last: Mutex<Option<RelayServingStatus>>,
    counters: Arc<PeerCounters>,
    /// True once something can hand this device circuits; **false** until a parked link is live.
    inbound_path: AtomicBool,
    /// Installed by the shell; `None` means not reachable.
    context: Mutex<Option<Arc<dyn ServingContext>>>,
    /// Back-reference so a reconcile can hand spawned tasks a `Weak`.
    me: OnceLock<Weak<RelayServingManager>>,
}

impl RelayServingManager {
    /// A fresh manager whose counter hook pushes status on link churn.
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

        // Weak, so a late hook on a dropped manager is a no-op.
        let weak = Arc::downgrade(&manager);
        manager.counters.set_on_change(Arc::new(move || {
            let Some(manager) = weak.upgrade() else {
                return;
            };
            // Fires from a pump task or `Drop`: must not block.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    manager.refresh().await;
                });
            }
        }));
        manager
    }

    /// The CURRENT status; re-probes and reconciles first.
    pub async fn status(&self) -> RelayServingStatus {
        let config = self.inner.lock().await.config;
        self.reconcile(config).await
    }

    /// Apply `config` live; idempotent.
    pub async fn apply(&self, config: RelayServingConfig) -> RelayServingStatus {
        self.reconcile(config).await
    }

    /// Reconcile and push an event if anything changed.
    pub async fn refresh(&self) {
        let _ = self.status().await;
    }

    /// Record whether anything can currently hand this device circuits.
    pub fn set_inbound_path(&self, present: bool) {
        self.inbound_path.store(present, Ordering::Relaxed);
    }

    /// Hand a link to the running node; refused (not queued) when none runs.
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

    /// Install the context; takes effect at the next reconcile.
    fn set_context(&self, context: Arc<dyn ServingContext>) {
        *self.context.lock().unwrap() = Some(context);
    }

    fn context(&self) -> Option<Arc<dyn ServingContext>> {
        self.context.lock().unwrap().clone()
    }

    /// Revocation store keyed on the pinned directory key.
    ///
    /// [`RevocationStore::unconfigured`] without a key: extending without revocation checks is the fail-open this prevents.
    fn revocation_store(&self) -> RevocationStore {
        match self.context().and_then(|c| c.directory()) {
            Some((_, key)) => RevocationStore::enforcing(key),
            None => RevocationStore::unconfigured(),
        }
    }

    /// Next-hop directory keyed on the same key: WHERE this device may extend.
    ///
    /// [`NextHopDirectory::unconfigured`] without a key, turning `allow_extend` off: never a general dialer.
    fn next_hop_directory(&self) -> NextHopDirectory {
        match self.context().and_then(|c| c.directory()) {
            Some((_, key)) => NextHopDirectory::enforcing(key),
            None => NextHopDirectory::unconfigured(),
        }
    }

    /// Start parking outbound connections for the running engine.
    ///
    /// `None` without a context: stays `Waiting{NoInboundPath}`.
    fn start_reachability(
        &self,
        leaf_der: pollis_relay::CertificateDer<'static>,
        revocations: RevocationStore,
        next_hops: NextHopDirectory,
    ) -> Option<Reachability> {
        let context = self.context()?;
        let me = self.me.get()?.clone();

        // Push status on reachability changes.
        let on_change: Arc<dyn Fn(usize) + Send + Sync> = {
            let me = me.clone();
            Arc::new(move |live: usize| {
                let Some(manager) = me.upgrade() else {
                    return;
                };
                manager.set_inbound_path(live > 0);
                // Must not block: hand the reconcile to the runtime.
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
            next_hops,
            leaf_der,
            acceptor: Arc::new(ManagerAcceptor(me)),
            on_change,
        }))
    }

    /// Probe, decide whether the node should run, make it so, report.
    async fn reconcile(&self, config: RelayServingConfig) -> RelayServingStatus {
        let link = platform::probe().await;
        // Taken out for shutdown, drained after the lock is released.
        let mut draining = None;
        let mut inner = self.inner.lock().await;
        inner.config = config;

        // Reachability doesn't gate the node; held true here.
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
            // Starts empty, so the node refuses to extend until a verified list arrives.
            let revocations = self.revocation_store();
            // The reachability loop installs the verified directory here too.
            let next_hops = self.next_hop_directory();
            match PeerEngine::start(
                self.counters.clone(),
                revocations.clone(),
                next_hops.clone(),
            ) {
                Ok(engine) => {
                    // Park with the engine's leaf: the cert is this peer's identity.
                    inner.reachability = self.start_reachability(
                        engine.cert_der().clone(),
                        revocations,
                        next_hops,
                    );
                    inner.engine = Some(engine);
                }
                Err(e) => {
                    // A failed start is never reported as serving.
                    crate::diag!("[peer-relay] could not start the peer node: {e}");
                }
            }
        } else if !engine_wanted && inner.engine.is_some() {
            self.inbound_path.store(false, Ordering::Relaxed);
            // Stop being reachable as soon as consent is gone.
            inner.reachability = None;
            draining = inner.engine.take();
        }

        let running = inner.engine.is_some();
        drop(inner);

        // Drain OUTSIDE the mutex: `shutdown` waits, and every inbound link's `attach_link` needs this lock.
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

    /// Push only on change; bytes forwarded is excluded to avoid an event storm.
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
                crate::diag!("[peer-relay] status event dropped: {e}");
            }
        }
    }
}

/// Hands a spliced link to whatever node the manager is currently running.
///
/// `Weak`, so a draining park task can't keep a manager alive.
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

    struct RecordingSink(Arc<StdMutex<Vec<RelayServingStatus>>>);

    impl EventSink<RelayServingStatus> for RecordingSink {
        fn send(&self, event: RelayServingStatus) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    /// Reports a directory but never an identity.
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

    /// Consent alone never reports `Serving` without reachability.
    #[tokio::test]
    async fn consent_settles_to_waiting_without_an_inbound_path() {
        let manager = RelayServingManager::new();
        let status = manager.apply(consent_without_conditions()).await;
        assert_eq!(status.state, RelayServingState::Waiting);
        assert_eq!(status.hold, Some(RelayServingHold::NoInboundPath));
        assert!(status.config.enabled);
    }

    /// The node runs during `Waiting{NoInboundPath}`; an inbound path flips the state.
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
        let (_client, peer_end) = super::super::link::loopback_pair();
        assert!(!manager.attach_link(peer_end).await);
        assert!(!manager.inbound_path.load(Ordering::Relaxed));
    }

    /// **Contract §5.** The engine gets the directory-backed store, keyed like the directory.
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
        assert!(
            manager.next_hop_directory().is_configured(),
            "a peer with a directory must be able to check where it extends to"
        );
    }

    /// No directory ⇒ no key ⇒ the store admits nothing.
    #[tokio::test]
    async fn without_a_directory_the_engine_cannot_evaluate_revocation() {
        let manager = RelayServingManager::new();
        assert!(!manager.revocation_store().is_configured());

        manager.set_context(Arc::new(FakeContext(None)));
        assert!(!manager.revocation_store().is_configured());
    }

    /// No directory key ⇒ `allow_extend` off.
    #[tokio::test]
    async fn without_a_directory_the_engine_will_not_extend_anywhere() {
        let manager = RelayServingManager::new();
        assert!(!manager.next_hop_directory().is_configured());

        manager.set_context(Arc::new(FakeContext(None)));
        assert!(!manager.next_hop_directory().is_configured());
    }

    /// No way to be reached settles on the reachability hold.
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

        manager.status().await;
        assert_eq!(events.lock().unwrap().len(), 1);

        manager.status().await;
        assert_eq!(events.lock().unwrap().len(), 1);

        manager.apply(consent_without_conditions()).await;
        let pushed = events.lock().unwrap().clone();
        assert_eq!(pushed.len(), 2);
        assert_eq!(pushed[1].state, RelayServingState::Waiting);
        assert_eq!(pushed[1].hold, Some(RelayServingHold::NoInboundPath));
    }
}
