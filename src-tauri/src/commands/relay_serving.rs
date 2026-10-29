// Generated shim file. Each #[tauri::command] forwards to
// pollis_core::commands::relay_serving::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;

#[cfg(feature = "native-shell")]
use tauri::{AppHandle, Emitter};

use crate::error::Result;
// The glob is the ONLY import from this module. Naming anything from it a
// second time makes that private import shadow the public re-export, so the
// name silently stops being visible to callers of this shim — the bug that
// once hid RELAY_SERVING_EVENT (the constant the renderer's event name must
// match) and, until clippy was armed (#912), was still hiding
// `set_status_sink`. The glob already puts both in scope here.
pub use pollis_core::commands::relay_serving::*;
use pollis_core::net::peer::{RelayServingConfig, RelayServingStatus};
use pollis_core::sink::EventSink;

#[tauri::command]
pub async fn get_relay_serving_status() -> Result<RelayServingStatus> {
    pollis_core::commands::relay_serving::get_relay_serving_status().await
}

#[tauri::command]
pub async fn set_relay_serving(config: RelayServingConfig) -> Result<RelayServingStatus> {
    pollis_core::commands::relay_serving::set_relay_serving(config).await
}

/// Pushes the status to every window as the global `relay-serving-status` event. Native shell only (`Wry`).
#[cfg(feature = "native-shell")]
struct AppHandleSink(AppHandle);

#[cfg(feature = "native-shell")]
impl EventSink<RelayServingStatus> for AppHandleSink {
    fn send(&self, event: RelayServingStatus) -> std::result::Result<(), String> {
        self.0
            .emit(RELAY_SERVING_EVENT, event)
            .map_err(|e| e.to_string())
    }
}

/// Wire the status event to the app handle; without it the status line goes stale.
#[cfg(feature = "native-shell")]
pub fn install_status_sink(app: AppHandle) {
    set_status_sink(Arc::new(AppHandleSink(app)));
}
