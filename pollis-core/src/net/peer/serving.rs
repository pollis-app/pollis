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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::sink::EventSink;

use super::conditions::{
    hold_for, RelayServingConfig, RelayServingStatus, ServingSignals,
};
use super::engine::{PeerCounters, PeerEngine};
use super::link::PeerLink;
use super::platform;

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

struct Inner {
    config: RelayServingConfig,
    engine: Option<PeerEngine>,
}

pub struct RelayServingManager {
    /// Async because reconciling starts and drains a node.
    inner: tokio::sync::Mutex<Inner>,
    sink: Mutex<Option<Arc<dyn EventSink<RelayServingStatus>>>>,
    /// The last status pushed, so an unchanged reconcile pushes nothing.
    last: Mutex<Option<RelayServingStatus>>,
    counters: Arc<PeerCounters>,
    /// True once something can hand this device circuits. Set by the transport
    /// that attaches the device to a first-party relay; **false** until then,
    /// which is why a consenting device today settles on
    /// `Waiting{NoInboundPath}` rather than claiming to be serving.
    inbound_path: AtomicBool,
}

impl RelayServingManager {
    /// A fresh manager, with the counter change-hook already wired so link
    /// churn pushes a status event.
    pub fn new() -> Arc<RelayServingManager> {
        let manager = Arc::new(RelayServingManager {
            inner: tokio::sync::Mutex::new(Inner {
                config: RelayServingConfig::default(),
                engine: None,
            }),
            sink: Mutex::new(None),
            last: Mutex::new(None),
            counters: PeerCounters::new(),
            inbound_path: AtomicBool::new(false),
        });

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

    /// The whole state machine in one place: probe, decide whether the node
    /// should be up, make that true, then report what is actually the case.
    async fn reconcile(&self, config: RelayServingConfig) -> RelayServingStatus {
        let link = platform::probe().await;
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
            // OPEN SEAM (#813): unconfigured means this engine will refuse every
            // `Extend`, so it carries nothing. That is the honest state today —
            // a peer is not reachable until the first-party reverse hop exists
            // and nothing publishes peers into the directory yet, so no circuit
            // can select one either. Wiring the client's directory-backed
            // `RevocationStore` in here is the last step of making peer relaying
            // live, and it must land with those two, not before: a peer that
            // forwards without being able to evaluate revocation is exactly the
            // fail-open this phase exists to prevent.
            match PeerEngine::start(
                self.counters.clone(),
                pollis_relay::policy::RevocationStore::unconfigured(),
            ) {
                Ok(engine) => {
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
            if let Some(mut engine) = inner.engine.take() {
                // Let someone else's in-flight message finish.
                engine.shutdown().await;
            }
        }

        let running = inner.engine.is_some();
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
        drop(inner);

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
