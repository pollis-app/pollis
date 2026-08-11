//! "Run a relay for others" — the command surface behind the Preferences
//! consent section (design `docs/relay-overlay-design.md` §10.2, #813).
//!
//! Deliberately NOT the same thing as `commands::overlay`: that decides whether
//! *your* traffic goes through the overlay, this decides whether *other
//! people's* traffic goes through *your* device. Neither implies the other.
//!
//! The engine lives in [`crate::net::peer`]; this module is the thin, typed
//! boundary the renderer calls. Consent is persisted by the UI in the synced
//! preferences blob and re-applied here on every toggle and once after
//! login/restart — the same shape `set_overlay_mode` has, for the same reason
//! (the backend applies, the settings slice persists).

use std::sync::Arc;

use crate::error::Result;
use crate::net::peer::{self, AppStateContext, RelayServingConfig, RelayServingStatus};
use crate::sink::EventSink;
use crate::state::AppState;

pub use crate::net::peer::RELAY_SERVING_EVENT;

/// The CURRENT live status: what this device is actually doing right now, which
/// may differ from what was last asked for (consent given on battery reports
/// `waiting` + `on_battery`). Never fails — "we cannot tell" is expressed in the
/// payload (`unsupported`, or a `null` platform signal), not as an error, so the
/// UI can always say something true.
pub async fn get_relay_serving_status() -> Result<RelayServingStatus> {
    Ok(peer::manager().status().await)
}

/// Apply `config` live, returning the status it settled on.
///
/// Idempotent. The settled status is the answer, not an echo of the request: it
/// is normal and correct for consent to settle on `waiting` rather than
/// `serving`, and the UI renders that difference rather than pretending the
/// toggle did what it said.
pub async fn set_relay_serving(config: RelayServingConfig) -> Result<RelayServingStatus> {
    Ok(peer::manager().apply(config).await)
}

/// Install the sink the [`RELAY_SERVING_EVENT`] event is pushed through.
///
/// Required, not optional: CLAUDE.md bans polling, so without this the status
/// line only refreshes when the component remounts. The desktop shell wires it
/// to `AppHandle::emit` during setup.
pub fn set_status_sink(sink: Arc<dyn EventSink<RelayServingStatus>>) {
    peer::set_event_sink(sink);
}

/// Hand the peer-serving manager the two app-shaped inputs a device needs to be
/// **reachable**: the signed relay directory (where the first-party relays it
/// parks at come from) and the device identity a parked connection
/// authenticates with.
///
/// Called once during setup, and deliberately separate from the two commands —
/// D2's command shape takes no state, and reachability is a property of the
/// device rather than of a call. Without it the device still runs its node,
/// simply parks nowhere, and reports `waiting` + `no_inbound_path` rather than
/// pretending to serve.
pub fn attach_app_state(state: &Arc<AppState>) {
    peer::set_serving_context(Arc::new(AppStateContext::new(state)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::peer::RelayServingState;

    /// The commands are callable with no app state and no login, and the
    /// out-of-the-box answer is off — consent is never implied.
    #[tokio::test]
    async fn status_is_readable_before_anything_is_configured() {
        let status = get_relay_serving_status().await.unwrap();
        assert!(!status.config.enabled);
        assert!(matches!(
            status.state,
            RelayServingState::Off | RelayServingState::Unsupported
        ));
    }
}
