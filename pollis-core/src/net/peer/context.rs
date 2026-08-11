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
use pollis_relay::DeviceCertMaterial;

use crate::state::AppState;

/// Identity version stamped into the locally-minted relay device cert. Mirrors
/// `net::overlay`'s constant — the relay verifies the chain offline, so both
/// call sites must mint the same shape.
const OVERLAY_CERT_IDENTITY_VERSION: u32 = 1;

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
        Ok(Arc::new(build_client_identity(&state).await?))
    }
}

/// Build this device's relay [`ClientIdentity`] from local state.
///
/// **This mirrors `net::overlay::build_client_identity`, which is private to a
/// file this phase does not own.** The two must stay identical: the relay
/// verifies one offline device-cert chain and does not care which side of the
/// app minted it. A cross-cutting request is filed to lift the original to
/// `pub(crate)` at integration so this copy can be deleted.
async fn build_client_identity(state: &Arc<AppState>) -> anyhow::Result<ClientIdentity> {
    let user_id = signing_user(state).await?;

    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("peer-relay: device_id not set (not logged in)"))?;

    // Account identity key (local): absent/locked ⇒ fail-closed (pre-enrollment).
    let account_key = crate::commands::account_identity::load_account_id_key(state, &user_id)
        .await
        .map_err(|e| anyhow::anyhow!("peer-relay: account identity unavailable ({e})"))?;

    // Device signing keys. The ML-DSA-44 one signs the handshake; the Ed25519
    // one is only carried, because the cert binds both. Scoped so the !Send
    // provider drops before any await.
    let (device_signing, ed_pub, pq_pub) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("peer-relay: not signed in (local DB closed)"))?;
        let provider = crate::commands::mls::PollisProvider::new(db.conn());
        let (ed_pub, pq_pub) =
            crate::commands::mls::load_device_cert_pubs(&provider, &user_id, &device_id)
                .map_err(|e| anyhow::anyhow!("peer-relay: device signing keys unavailable ({e})"))?;
        let (device_signing, _) =
            crate::commands::mls::load_device_pq_signing_key(&provider, &user_id, &device_id)
                .map_err(|e| anyhow::anyhow!("peer-relay: device PQ key unavailable ({e})"))?;
        (device_signing, ed_pub, pq_pub)
    };
    let ed_pub: [u8; 32] = ed_pub
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("peer-relay: device Ed25519 pub is not 32 bytes"))?;

    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cert = DeviceCertMaterial::mint(
        &account_key,
        &device_id,
        &ed_pub,
        &pq_pub,
        OVERLAY_CERT_IDENTITY_VERSION,
        issued_at,
    );

    Ok(ClientIdentity::new(
        user_id,
        device_id,
        ed_pub,
        device_signing,
        cert,
    ))
}

/// The user this device signs as: prefer the unlocked session, fall back to the
/// accounts index before unlock. Mirrors `net::overlay::overlay_signing_user`.
async fn signing_user(state: &Arc<AppState>) -> anyhow::Result<String> {
    if let Some(u) = state.unlock.lock().await.as_ref() {
        if !u.user_id.is_empty() {
            return Ok(u.user_id.clone());
        }
    }
    let index = crate::accounts::read_accounts_index()
        .map_err(|e| anyhow::anyhow!("peer-relay: read accounts index: {e}"))?;
    index
        .last_active_user
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("peer-relay: no active user to sign the park handshake"))
}
