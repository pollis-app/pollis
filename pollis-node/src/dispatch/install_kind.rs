// Port of `src-tauri/src/commands/install_kind.rs`. The Tauri implementation
// reads `tauri::utils::config::BundleType` to decide whether the running
// binary is AUR-packaged. That API does not exist post-port. Phase 5
// (electron-builder) will surface install kind via an env var / bundled JSON
// / filesystem probe and we'll wire it in then. Until then, the command
// returns a clear error so frontend probes don't silently get a `None`
// (which would imply "auto-updater is OK to run") and unmask the pacman
// install we explicitly want to block.

use napi::bindgen_prelude::*;

pub async fn dispatch(
    cmd: &str,
    _args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "detect_managed_install" => Some(Err(Error::from_reason(
            "Phase 5: install_kind needs Electron-side replacement".to_string(),
        ))),
        _ => None,
    }
}
