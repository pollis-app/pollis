//! Runtime application of the closed-overlay relay mode (design
//! `docs/relay-overlay-design.md` §14, `docs/relay-operations.md`).
//!
//! Applies the mode live, no restart; `set_overlay_mode` is idempotent.
//!
//! **Off is sacred.** With the overlay off no shim runs; every path is direct.

use std::sync::Arc;

use pollis_relay::OverlayMode;

use crate::error::{Error, Result};
use crate::state::AppState;

/// The CURRENT live overlay mode: the running shim's mode, or `off` when no shim
/// is running. Returned as `"off"` | `"prefer"` | `"strict"`.
pub async fn get_overlay_mode(state: &Arc<AppState>) -> Result<String> {
    Ok(mode_to_str(current_mode(state)))
}

/// Parse `mode` (`off`|`prefer`|`strict`, any case) and apply it live.
/// Unknown values are a hard error (unlike the env parse); doesn't persist.
pub async fn set_overlay_mode(state: &Arc<AppState>, mode: String) -> Result<()> {
    let mode = parse_mode(&mode)?;
    apply_overlay_mode(state, mode).await
}

/// Drive the overlay to `mode` live; shared with boot (`POLLIS_OVERLAY`). Idempotent.
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
                // Defensive: handle gone despite non-off; rebuild.
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

/// Off → non-off. Start the shim and publish it. On failure leave the previous
/// (working, direct) state intact and surface the error — never a half-routed
/// app (§10.1, Strict must not silent-direct).
async fn start_and_route(state: &Arc<AppState>, mode: OverlayMode) -> Result<()> {
    let handle = crate::net::overlay::start_overlay_shim(state, mode).await?;
    *state.overlay.lock().unwrap() = Some(Arc::new(handle));
    Ok(())
}

/// non-off → Off. Dropping the stored handle aborts the shim's accept loop, and
/// every caller's next `overlay_handle()` returns `None` — direct again.
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
