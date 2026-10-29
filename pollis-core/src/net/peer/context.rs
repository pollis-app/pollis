//! What the serving manager needs from the rest of the app, behind one trait.
//!
//! Relay serving is a process singleton ([`super::serving`]) whose commands take
//! no `AppState`, but becoming reachable needs the signed directory and the
//! device's relay [`ClientIdentity`]. The desktop shell installs the real
//! [`ServingContext`] ([`crate::commands::relay_serving::attach_app_state`]);
//! without one the node runs, parks nowhere, and reports
//! `Waiting{NoInboundPath}`.

use std::sync::{Arc, Weak};

use pollis_relay::client::ClientIdentity;

use crate::state::AppState;

/// The app-shaped inputs a serving device needs to become reachable.
#[async_trait::async_trait]
pub trait ServingContext: Send + Sync + 'static {
    /// `(directory_url, pinned_key_b64)`, or `None` when this build has no
    /// signed directory (the device then stays unreachable).
    fn directory(&self) -> Option<(String, String)>;

    /// The device identity a parked connection authenticates with. Fails before
    /// login, so a device that cannot prove who it is does not park.
    async fn identity(&self) -> anyhow::Result<Arc<ClientIdentity>>;
}

/// The production context, backed by the live [`AppState`]. Holds a `Weak`, so
/// a manager outliving the app state degrades to "no context".
pub struct AppStateContext {
    state: Weak<AppState>,
    directory: Option<(String, String)>,
}

impl AppStateContext {
    pub fn new(state: &Arc<AppState>) -> AppStateContext {
        let directory = match (
            state.config.overlay_directory_url.clone(),
            state.config.overlay_directory_key.clone(),
        ) {
            (Some(url), Some(key)) if !url.is_empty() && !key.is_empty() => Some((url, key)),
            _ => None,
        };
        AppStateContext {
            state: Arc::downgrade(state),
            directory,
        }
    }
}

#[async_trait::async_trait]
impl ServingContext for AppStateContext {
    fn directory(&self) -> Option<(String, String)> {
        self.directory.clone()
    }

    async fn identity(&self) -> anyhow::Result<Arc<ClientIdentity>> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("peer-relay: app state is gone"))?;
        // The overlay's minter, not a copy, so it cannot drift from what the
        // relay accepts.
        Ok(Arc::new(
            crate::net::overlay::build_client_identity(&state, "peer-relay").await?,
        ))
    }
}
