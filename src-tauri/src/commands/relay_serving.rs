// Generated shim file. Each #[tauri::command] forwards to
// pollis_core::commands::relay_serving::*. Edit pollis-core, not here.
// Not a forward: the status sink adapter, since pollis-core stays
// Tauri-runtime-free.

#![allow(unused_imports)]
use std::sync::Arc;

#[cfg(feature = "native-shell")]
use tauri::{AppHandle, Emitter};

use crate::error::Result;
// Must stay the ONLY import from this module: naming anything from it again
// shadows the public re-export and hides it from callers (#912).
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

/// Pushes the status to every window as the global `relay-serving-status`
/// event. Native shell only (`AppHandle` defaults to `Wry`).
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

/// Wire the status event to the app handle; without it the status line only
/// refreshes on remount (no polling).
#[cfg(feature = "native-shell")]
pub fn install_status_sink(app: AppHandle) {
    set_status_sink(Arc::new(AppHandleSink(app)));
}
