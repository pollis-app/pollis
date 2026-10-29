// Generated shim file. Each #[tauri::command] forwards to
// pollis_core::commands::autolock::*. Edit pollis-core, not here.
// Not a forward: the lock sink adapter, since pollis-core stays
// Tauri-runtime-free.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

#[cfg(feature = "native-shell")]
use tauri::{AppHandle, Emitter};

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::autolock::*;
use pollis_core::sink::EventSink;

#[tauri::command]
pub async fn set_auto_lock_timeout(
    state: State<'_, Arc<AppState>>,
    minutes: Option<u32>,
) -> Result<()> {
    pollis_core::commands::autolock::set_auto_lock_timeout(&state, minutes).await
}

#[tauri::command]
pub async fn report_user_activity() -> Result<()> {
    pollis_core::commands::autolock::report_user_activity().await
}

/// Pushes the lock to every window as the global `auto-lock` event. Native
/// shell only. Required: without a sink the backend locks but the renderer is
/// never told.
#[cfg(feature = "native-shell")]
struct AppHandleSink(AppHandle);

#[cfg(feature = "native-shell")]
impl EventSink<()> for AppHandleSink {
    fn send(&self, _event: ()) -> std::result::Result<(), String> {
        self.0
            .emit(AUTO_LOCK_EVENT, ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(feature = "native-shell")]
pub fn install_auto_lock_sink(app: AppHandle) {
    set_event_sink(Arc::new(AppHandleSink(app)));
}
