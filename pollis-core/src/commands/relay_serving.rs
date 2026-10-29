//! "Run a relay for others" — the command surface behind the Preferences
//! consent section (design `docs/relay-overlay-design.md` §10.2, #813).
//! Not `commands::overlay`: that routes *your* traffic; this lets *others'* traffic use your device.

use std::sync::Arc;

use crate::error::Result;
use crate::net::peer::{self, AppStateContext, RelayServingConfig, RelayServingStatus};
use crate::sink::EventSink;
use crate::state::AppState;

pub use crate::net::peer::RELAY_SERVING_EVENT;

/// The CURRENT live status: what this device is actually doing right now, which
/// may differ from what was last asked for. Never fails: unknowns are in the payload.
pub async fn get_relay_serving_status() -> Result<RelayServingStatus> {
    Ok(peer::manager().status().await)
}

/// Apply `config` live, returning the status it settled on.
pub async fn set_relay_serving(config: RelayServingConfig) -> Result<RelayServingStatus> {
    Ok(peer::manager().apply(config).await)
}

/// Install the sink the [`RELAY_SERVING_EVENT`] event is pushed through.
/// Required: polling is banned, so this is how the status line refreshes.
pub fn set_status_sink(sink: Arc<dyn EventSink<RelayServingStatus>>) {
    peer::set_event_sink(sink);
}

/// Hand the peer-serving manager the two app-shaped inputs a device needs to be
/// **reachable**: the signed relay directory (where the first-party relays it
/// parks at come from) and the device identity a parked connection
/// authenticates with.
/// Called once during setup; without it the device parks nowhere (`no_inbound_path`).
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
