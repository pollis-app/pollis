//! What the serving manager needs from the rest of the app, behind one trait.
//!
//! Serving is a process singleton with no `AppState`, but reachability needs two things only the app has:
//!
//! - the **signed directory** (URL + pinned key), the only channel peer identities travel;
//! - the device's **relay [`ClientIdentity`]** for the park handshake.
//!
//! The shell installs [`crate::commands::relay_serving::attach_app_state`]; without one the node parks nowhere and reports `Waiting{NoInboundPath}`.

use std::sync::{Arc, Weak};

use pollis_relay::client::ClientIdentity;

use crate::state::AppState;

/// The app-shaped inputs a serving device needs to become reachable.
#[async_trait::async_trait]
pub trait ServingContext: Send + Sync + 'static {
    /// `(directory_url, pinned_key_b64)`, or `None`; without one the device stays unreachable.
    fn directory(&self) -> Option<(String, String)>;

    /// The identity a parked connection authenticates with. Fails closed before login.
    async fn identity(&self) -> anyhow::Result<Arc<ClientIdentity>>;
}

/// The production context, backed by the live [`AppState`].
///
/// `Weak`, so an outliving manager degrades to "no context".
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
        // The overlay's own minter, so the cert can't drift from what the relay accepts.
        Ok(Arc::new(
            crate::net::overlay::build_client_identity(&state, "peer-relay").await?,
        ))
    }
}
