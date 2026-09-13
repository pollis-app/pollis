use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::state::AppState;

pub async fn mark_update_required(state: &Arc<AppState>) -> Result<()> {
    state.update_required.store(true, Ordering::Relaxed);
    Ok(())
}

pub async fn is_update_required(state: &Arc<AppState>) -> Result<bool> {
    Ok(state.update_required.load(Ordering::Relaxed))
}

/// How the auto-updater is allowed to reach `cdn.pollis.com` right now.
///
/// The updater is the one HTTP caller in the app that does not go through
/// `pollis_relay::http::http_client` — it is `tauri-plugin-updater`'s own
/// `reqwest` client, built inside the plugin. So with the overlay on, every
/// other first-party request rode the relay and this one still went direct,
/// announcing the device's real address to the CDN on every window focus. In
/// `Strict` that is precisely the silent-direct the mode exists to forbid
/// (design §10.1).
///
/// Mirrored in TypeScript by `UpdateCheckPlan` in
/// `frontend/src/bridge/updater.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateCheckPlan {
    /// The overlay is off. Check directly, exactly as a build without the
    /// overlay does.
    Direct,
    /// Route the manifest fetch AND the artifact download through the shim.
    /// `tauri-plugin-updater`'s `check` takes a proxy and stores it on the
    /// `Update` it returns, so the download inherits it.
    Proxy { url: String },
    /// `Strict` with no circuit: the check does not happen. Degrading to a
    /// direct fetch here would leak the address the mode exists to hide, and an
    /// update check is the one first-party request that can always wait — the
    /// next focus after the overlay comes up runs it.
    Blocked { reason: String },
}

/// Decide the [`UpdateCheckPlan`] from the live overlay state.
///
/// Split from the command so the four cases are unit-testable without an
/// `AppState`: the input really is just "which mode, and is a shim up".
pub fn plan_update_check(
    mode: pollis_relay::OverlayMode,
    socks_addr: Option<std::net::SocketAddr>,
) -> UpdateCheckPlan {
    match (mode, socks_addr) {
        (pollis_relay::OverlayMode::Off, _) => UpdateCheckPlan::Direct,
        (_, Some(addr)) => UpdateCheckPlan::Proxy {
            url: format!("socks5h://{addr}"),
        },
        // Prefer without a circuit is allowed to go direct — that is what
        // Prefer MEANS. Strict is not.
        (pollis_relay::OverlayMode::Prefer, None) => UpdateCheckPlan::Direct,
        (pollis_relay::OverlayMode::Strict, None) => UpdateCheckPlan::Blocked {
            reason: "the overlay is in strict mode and no relay circuit is up, so the \
                     update check would have to go direct — it is skipped instead"
                .to_string(),
        },
    }
}

/// The live plan for this device.
pub async fn get_update_check_plan(state: &Arc<AppState>) -> Result<UpdateCheckPlan> {
    let handle = state.overlay_handle();
    let mode = handle
        .as_ref()
        .map(|h| h.mode())
        .unwrap_or(pollis_relay::OverlayMode::Off);
    let addr = handle.as_ref().map(|h| h.socks_addr());
    Ok(plan_update_check(mode, addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_relay::OverlayMode;
    use std::net::SocketAddr;

    fn addr() -> SocketAddr {
        "127.0.0.1:9150".parse().unwrap()
    }

    #[test]
    fn off_checks_directly() {
        assert_eq!(plan_update_check(OverlayMode::Off, None), UpdateCheckPlan::Direct);
        // Off is off even if a handle somehow lingers.
        assert_eq!(
            plan_update_check(OverlayMode::Off, Some(addr())),
            UpdateCheckPlan::Direct
        );
    }

    #[test]
    fn a_live_circuit_carries_the_update_check() {
        for mode in [OverlayMode::Prefer, OverlayMode::Strict] {
            assert_eq!(
                plan_update_check(mode, Some(addr())),
                UpdateCheckPlan::Proxy {
                    url: "socks5h://127.0.0.1:9150".to_string()
                },
                "{mode:?} with a shim up must route the updater through it"
            );
        }
    }

    #[test]
    fn prefer_without_a_circuit_falls_back_to_direct() {
        assert_eq!(
            plan_update_check(OverlayMode::Prefer, None),
            UpdateCheckPlan::Direct
        );
    }

    /// The finding: in Strict, an update check must never be the one request
    /// that quietly goes direct and hands the CDN the real address.
    #[test]
    fn strict_without_a_circuit_blocks_rather_than_leaking() {
        let plan = plan_update_check(OverlayMode::Strict, None);
        assert!(
            matches!(plan, UpdateCheckPlan::Blocked { .. }),
            "strict with no circuit must block, got {plan:?}"
        );
        assert_ne!(plan, UpdateCheckPlan::Direct);
    }
}
