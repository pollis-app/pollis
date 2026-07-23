//! Runtime application of the closed-overlay relay mode (design
//! `docs/relay-overlay-design.md` §14, `docs/relay-operations.md`).
//!
//! This is the runtime-application engine: flipping the mode here ACTUALLY routes
//! (or stops routing) first-party control-plane traffic through the relay overlay,
//! live, with no app restart. `set_overlay_mode` is idempotent and safe to call on
//! every app start / login — the settings-UI slice calls it after loading the
//! saved preference; boot calls [`apply_overlay_mode`] with `POLLIS_OVERLAY`.
//!
//! **Off is sacred.** With the overlay off no shim runs and every network path is
//! byte-for-byte the pre-overlay direct behavior (state.overlay `None`, remote DBs
//! built without the connector).
//!
//! ## The live-apply state machine
//!
//! - **Off → non-off**: build the [`RealRelayFactory`](crate::net::overlay) +
//!   start the loopback SOCKS5 shim, publish the handle on `AppState.overlay` (so
//!   every `http_client` caller routes through it), then reconnect BOTH remote DBs
//!   through the shim so libsql routes too. On failure, roll back to direct.
//! - **non-off → Off**: reconnect the remote DBs DIRECT first (so no in-flight
//!   libsql build races a dying shim), then drop the handle — which aborts the
//!   shim task.
//! - **Prefer ↔ Strict**: both non-off with the shim already running — flip the
//!   shim's routing policy mode live (a shared atomic the shim reads per request).
//!   No shim restart, no DB reconnect.

use std::net::SocketAddr;
use std::sync::Arc;

use pollis_relay::OverlayMode;

use crate::error::{Error, Result};
use crate::state::AppState;

/// The CURRENT live overlay mode: the running shim's mode, or `off` when no shim
/// is running. Returned as `"off"` | `"prefer"` | `"strict"`.
pub async fn get_overlay_mode(state: &Arc<AppState>) -> Result<String> {
    Ok(mode_to_str(current_mode(state)))
}

/// Parse `mode` (`"off"` | `"prefer"` | `"strict"`, case-insensitive) and APPLY
/// it live. A no-op when the mode is unchanged. Unlike the fail-safe env parse,
/// an unknown value here is a hard error — the UI passes a known value and a typo
/// should surface, not silently disable the overlay. Persisting the choice is the
/// UI slice's job; this only applies.
pub async fn set_overlay_mode(state: &Arc<AppState>, mode: String) -> Result<()> {
    let mode = parse_mode(&mode)?;
    apply_overlay_mode(state, mode).await
}

/// Drive the overlay to `mode`, live. Shared by [`set_overlay_mode`] and boot
/// (the shell calls this with `config.overlay_mode` right after wrapping the
/// `AppState` in an `Arc`, so `POLLIS_OVERLAY` is honored through the SAME code
/// path a settings toggle uses). Idempotent.
pub async fn apply_overlay_mode(state: &Arc<AppState>, mode: OverlayMode) -> Result<()> {
    let current = current_mode(state);
    if current == mode {
        return Ok(());
    }
    match (current, mode) {
        // Off → non-off: stand the overlay up and route everything through it.
        (OverlayMode::Off, _) => start_and_route(state, mode).await,
        // non-off → Off: tear the overlay down and go back to direct.
        (_, OverlayMode::Off) => stop_and_go_direct(state).await,
        // Prefer ↔ Strict: flip the live policy mode; no restart, no reconnect.
        (_, _) => {
            match state.overlay_handle() {
                Some(handle) => {
                    handle.set_mode(mode);
                    Ok(())
                }
                // Defensive: `current` said non-off but the handle is gone.
                // Rebuild rather than leave the app half-routed.
                None => start_and_route(state, mode).await,
            }
        }
    }
}

/// The live mode: the running shim's, or `Off` when no shim is up.
fn current_mode(state: &Arc<AppState>) -> OverlayMode {
    state
        .overlay_handle()
        .map(|h| h.mode())
        .unwrap_or(OverlayMode::Off)
}

/// Off → non-off. Start the shim, publish it, then route the DBs through it. On
/// any failure leave the previous (working, direct) state intact and surface the
/// error — never a half-routed app (§10.1, Strict must not silent-direct).
async fn start_and_route(state: &Arc<AppState>, mode: OverlayMode) -> Result<()> {
    let handle = crate::net::overlay::start_overlay_shim(state, mode).await?;
    let shim_addr = handle.socks_addr();

    // Publish first so the http_client hot path picks up the shim immediately.
    *state.overlay.lock().unwrap() = Some(Arc::new(handle));

    // Then route libsql (both remote DBs) through the shim.
    if let Err(e) = reconnect_dbs(state, Some(shim_addr)).await {
        // Roll back: drop the handle (aborts the shim task) and restore direct.
        *state.overlay.lock().unwrap() = None;
        let _ = reconnect_dbs(state, None).await;
        return Err(e);
    }
    Ok(())
}

/// non-off → Off. Reconnect the DBs DIRECT first so an in-flight libsql rebuild
/// can't race the shim task's abort, then drop the handle.
async fn stop_and_go_direct(state: &Arc<AppState>) -> Result<()> {
    reconnect_dbs(state, None).await?;
    // Dropping the stored handle aborts the shim's accept loop.
    *state.overlay.lock().unwrap() = None;
    Ok(())
}

/// Point (or unpoint) both remote DBs at the shim. `log_db` is often the very
/// same `Arc` as `remote_db` (unconfigured commit-log DB) — skip the duplicate.
async fn reconnect_dbs(state: &Arc<AppState>, shim: Option<SocketAddr>) -> Result<()> {
    state.remote_db.set_overlay_shim(shim).await?;
    if !Arc::ptr_eq(&state.remote_db, &state.log_db) {
        state.log_db.set_overlay_shim(shim).await?;
    }
    Ok(())
}

fn parse_mode(s: &str) -> Result<OverlayMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Ok(OverlayMode::Off),
        "prefer" => Ok(OverlayMode::Prefer),
        "strict" => Ok(OverlayMode::Strict),
        other => Err(Error::Config(format!(
            "invalid overlay mode {other:?} (expected off | prefer | strict)"
        ))),
    }
}

fn mode_to_str(mode: OverlayMode) -> String {
    match mode {
        OverlayMode::Off => "off",
        OverlayMode::Prefer => "prefer",
        OverlayMode::Strict => "strict",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_accepts_known_rejects_unknown() {
        assert_eq!(parse_mode("off").unwrap(), OverlayMode::Off);
        assert_eq!(parse_mode(" Prefer ").unwrap(), OverlayMode::Prefer);
        assert_eq!(parse_mode("STRICT").unwrap(), OverlayMode::Strict);
        assert!(parse_mode("bogus").is_err());
        assert!(parse_mode("").is_err());
    }

    #[test]
    fn mode_to_str_roundtrips() {
        for m in [OverlayMode::Off, OverlayMode::Prefer, OverlayMode::Strict] {
            assert_eq!(parse_mode(&mode_to_str(m)).unwrap(), m);
        }
    }
}
