//! What the serving manager needs from the rest of the app, behind one trait.
//!
//! Relay serving is a **process** singleton (see [`super::serving`]) — it
//! survives login, does not fan out per account, and its commands deliberately
//! take no `AppState` (that shape is D2's, mirrored byte-for-byte in
//! `frontend/src/hooks/queries/useRelayServing.ts`). But making the device
//! *reachable* needs two things that only the app has:
//!
//! - the **signed directory** (URL + the pinned Ed25519 key), which is where the
//!   first-party relays to park at come from, and the only channel peer-relay
//!   identities are ever allowed to travel over;
//! - the device's **relay [`ClientIdentity`]**, because a parked connection runs
//!   the ordinary device handshake before it registers (contract §1: no new
//!   auth).
//!
//! Rather than reach into `AppState` from a singleton, the manager holds a
//! [`ServingContext`]. The desktop shell installs the real one
//! ([`crate::commands::relay_serving::attach_app_state`]); a build that never
//! installs one simply has no directory and no identity, so it runs the node,
//! parks nowhere, and honestly reports `Waiting{NoInboundPath}` — the same state
//! the feature had before this wave, rather than a crash or a lie. Tests install
//! their own.

use std::sync::{Arc, Weak};

use pollis_relay::client::ClientIdentity;

use crate::state::AppState;

/// The app-shaped inputs a serving device needs to become reachable.
#[async_trait::async_trait]
pub trait ServingContext: Send + Sync + 'static {
    /// `(directory_url, pinned_key_b64)`, or `None` when this build has no
    /// signed directory. Without one there is nowhere to park and no key to
    /// evaluate revocation with, and both of those correctly leave the device
    /// unreachable rather than guessing.
    fn directory(&self) -> Option<(String, String)>;

    /// The device identity a parked connection authenticates with. Fails before
    /// login (no device cert exists yet), which is the fail-closed answer:
    /// a device that cannot prove who it is does not get to park.
    async fn identity(&self) -> anyhow::Result<Arc<ClientIdentity>>;
}

/// The production context, backed by the live [`AppState`].
///
/// Holds a `Weak` so a serving manager that outlives the app state (shutdown
/// ordering, tests) degrades to "no context" instead of keeping the whole app
/// alive.
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
        // The overlay's minter, not a copy of it: the relay verifies one offline
        // device-cert chain and does not care which side of the app minted it,
        // so a second implementation could only ever drift out of agreement
        // with the one the relay actually accepts.
        Ok(Arc::new(
            crate::net::overlay::build_client_identity(&state, "peer-relay").await?,
        ))
    }
}
