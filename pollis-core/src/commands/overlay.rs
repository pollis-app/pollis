//! Runtime application of the closed-overlay relay mode
//! (`docs/relay-overlay-design.md` §14): flipping the mode here live-routes (or
//! stops routing) first-party control-plane traffic through the relay, with no
//! restart.
//!
//! With the overlay off no shim runs and every network path is the plain direct
//! one.

use std::sync::Arc;

use pollis_relay::OverlayMode;

use crate::error::{Error, Result};
use crate::state::AppState;

/// The live overlay mode: `"off"` | `"prefer"` | `"strict"`.
pub async fn get_overlay_mode(state: &Arc<AppState>) -> Result<String> {
    Ok(mode_to_str(current_mode(state)))
}

/// Parse `mode` (case-insensitive) and apply it live. Unlike the fail-safe env
/// parse, an unknown value is an error so a typo can't silently disable the
/// overlay. The UI persists the choice.
pub async fn set_overlay_mode(state: &Arc<AppState>, mode: String) -> Result<()> {
    let mode = parse_mode(&mode)?;
    apply_overlay_mode(state, mode).await
}

/// Drive the overlay to `mode`, live. Boot calls this with `POLLIS_OVERLAY` so it
/// takes the same path as a settings toggle. Idempotent.
pub async fn apply_overlay_mode(state: &Arc<AppState>, mode: OverlayMode) -> Result<()> {
    let current = current_mode(state);
    if current == mode {
        return Ok(());
    }
    match (current, mode) {
        (OverlayMode::Off, _) => start_and_route(state, mode).await,
        (_, OverlayMode::Off) => stop_and_go_direct(state).await,
        // Prefer and Strict: flip the shim's policy live; no restart.
        (_, _) => {
            match state.overlay_handle() {
                Some(handle) => {
                    handle.set_mode(mode);
                    Ok(())
                }
                // `current` said non-off but the handle is gone: rebuild rather than leave the
                // app half-routed.
                None => start_and_route(state, mode).await,
            }
        }
    }
}

fn current_mode(state: &Arc<AppState>) -> OverlayMode {
    state
        .overlay_handle()
        .map(|h| h.mode())
        .unwrap_or(OverlayMode::Off)
}

/// Off to non-off. On failure the previous direct state is left intact and the
/// error surfaces, never a half-routed app (§10.1). Publishing the handle is the
/// routing: every overlaid caller reads `overlay_handle()` per request.
async fn start_and_route(state: &Arc<AppState>, mode: OverlayMode) -> Result<()> {
    let handle = crate::net::overlay::start_overlay_shim(state, mode).await?;
    *state.overlay.lock().unwrap() = Some(Arc::new(handle));
    Ok(())
}

/// Dropping the handle aborts the shim; every caller's next `overlay_handle()`
/// returns `None`.
async fn stop_and_go_direct(state: &Arc<AppState>) -> Result<()> {
    *state.overlay.lock().unwrap() = None;
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
