// Generated shim file. Each #[tauri::command] forwards to
// pollis_core::commands::relay_serving::*. Edit pollis-core, not here.
//
// The one thing that is NOT a forward is the status sink: pollis-core stays
// Tauri-runtime-free (`pollis_core::sink::EventSink`), so the adapter that turns
// a pushed status into a global `AppHandle::emit` lives on this side.

#![allow(unused_imports)]
use std::sync::Arc;

#[cfg(feature = "native-shell")]
use tauri::{AppHandle, Emitter};

use crate::error::Result;
pub use pollis_core::commands::relay_serving::*;
use pollis_core::commands::relay_serving::{set_status_sink, RELAY_SERVING_EVENT};
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

/// Pushes the status to every window as the global `relay-serving-status` event.
///
/// Gated on the native shell: `AppHandle` defaults to the `Wry` runtime, which a
/// headless (`--no-default-features`) build does not compile. The two commands
/// above stay ungated — they work fine with no sink, the UI just falls back to
/// reading the status on mount.
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

/// Wire the status event to the app handle. Called once during setup — without
/// it the status line only refreshes when the Preferences component remounts,
/// since CLAUDE.md bans polling it.
#[cfg(feature = "native-shell")]
pub fn install_status_sink(app: AppHandle) {
    set_status_sink(Arc::new(AppHandleSink(app)));
}
