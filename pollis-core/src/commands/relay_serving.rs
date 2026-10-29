//! "Run a relay for others" (#813, `docs/relay-overlay-design.md` §10.2): whether
//! *other people's* traffic goes through this device. Independent of
//! `commands::overlay`. The engine is [`crate::net::peer`]; the UI persists
//! consent and re-applies it here on toggle and after login.

use std::sync::Arc;

use crate::error::Result;
use crate::net::peer::{self, AppStateContext, RelayServingConfig, RelayServingStatus};
use crate::sink::EventSink;
use crate::state::AppState;

pub use crate::net::peer::RELAY_SERVING_EVENT;

/// What this device is actually doing now, which may differ from the request
/// (consent on battery reports `waiting`). Never fails: "unknown" is expressed in
/// the payload so the UI can always say something true.
pub async fn get_relay_serving_status() -> Result<RelayServingStatus> {
    Ok(peer::manager().status().await)
}

/// Apply `config` live and return the status it settled on (often `waiting`, not
/// `serving`). Idempotent.
pub async fn set_relay_serving(config: RelayServingConfig) -> Result<RelayServingStatus> {
    Ok(peer::manager().apply(config).await)
}

/// Install the sink [`RELAY_SERVING_EVENT`] is pushed through. Required: without
/// it the status only refreshes on remount (no polling).
pub fn set_status_sink(sink: Arc<dyn EventSink<RelayServingStatus>>) {
    peer::set_event_sink(sink);
}

/// Give the peer manager what it needs to be reachable: the signed relay directory
/// and the device identity. Called once at setup. Without it the device parks
/// nowhere and reports `waiting` + `no_inbound_path`.
pub fn attach_app_state(state: &Arc<AppState>) {
    peer::set_serving_context(Arc::new(AppStateContext::new(state)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::peer::RelayServingState;

    /// Callable with no app state or login, and the default is off.
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
