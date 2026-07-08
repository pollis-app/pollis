//! Background refresh of the client's read-only Turso token (#393).
//!
//! The client used to ship a long-lived read-only Turso token baked into the
//! bundle. This module keeps `remote_db` on a **DS-minted, short-TTL** read-only
//! token instead: on sign-in we spawn a loop that mints one via the Delivery
//! Service (`POST /v1/turso/token`), installs it on `remote_db`, and refreshes it
//! before it expires. A leaked client token's blast radius shrinks from "forever"
//! to the TTL.
//!
//! **Load-bearing, so it fails soft.** Unlike LiveKit/R2 (whose brokers just
//! disable a feature when unconfigured), Turso reads power the whole app. So any
//! mint failure — a DS with no Turso Platform credentials (503), a transient
//! network error — leaves the baked read-only token in place (see
//! `RemoteDb::set_remote_token` / `reconnect`), and reads keep working. The baked
//! token can only be dropped from the bundle once DS minting is live in prod.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

/// Process-global guard so re-sign-in doesn't stack multiple refresh loops.
/// `AppState` is itself a process singleton, so a static is equivalent to an
/// instance field here and avoids threading a flag through every constructor.
static REFRESH_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Long back-off after a mint failure so an unconfigured DS isn't hammered
/// (the baked read-only token stays in use in the meantime).
const RETRY_BACKOFF_SECS: u64 = 600;
/// Never re-mint more often than this, even for a pathologically small TTL.
const MIN_REFRESH_SECS: u64 = 60;

/// Spawn the read-only-token refresh loop for this session. Idempotent per
/// process (subsequent calls while a loop is live are no-ops). Best-effort and
/// self-terminating: the loop exits once the user logs out (device id cleared).
pub fn spawn_turso_token_refresh(state: &Arc<AppState>) {
    if REFRESH_ACTIVE.swap(true, Ordering::AcqRel) {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        loop {
            // Stop once the user has logged out — nothing to sign a mint with.
            if state.device_id.lock().await.is_none() {
                REFRESH_ACTIVE.store(false, Ordering::Release);
                return;
            }

            let sleep_secs = match crate::commands::mls::ds_turso_token(&state).await {
                Ok((token, expires_in)) => {
                    match state.remote_db.set_remote_token(token).await {
                        Ok(()) => {
                            eprintln!("[turso] installed DS-minted read-only token (ttl {expires_in}s)")
                        }
                        Err(e) => eprintln!("[turso] set token failed, keeping current: {e}"),
                    }
                    // Refresh at ~80% of the TTL so a new token is in place before
                    // the old one lapses; floor so a tiny TTL can't spin.
                    (expires_in.saturating_mul(4) / 5).max(MIN_REFRESH_SECS)
                }
                Err(e) => {
                    // 503 (DS has no Turso Platform creds) or a transient error:
                    // keep the baked read-only token and back off.
                    eprintln!("[turso] mint failed, using baked read-only token: {e}");
                    RETRY_BACKOFF_SECS
                }
            };

            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
        }
    });
}
