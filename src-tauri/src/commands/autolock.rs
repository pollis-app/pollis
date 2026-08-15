// Generated shim file. Each #[tauri::command] forwards to
// pollis_core::commands::autolock::*. Edit pollis-core, not here.
//
// The one thing that is NOT a forward is the lock sink: pollis-core stays
// Tauri-runtime-free (`pollis_core::sink::EventSink`), so the adapter that turns
// "the idle deadline expired" into a global `AppHandle::emit` lives on this side.

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

/// Pushes the lock to every window as the global `auto-lock` event.
///
/// Gated on the native shell for the same reason as the relay-serving sink:
/// `AppHandle` defaults to the `Wry` runtime, which a headless
/// (`--no-default-features`) build does not compile. The two commands above stay
/// ungated — without a sink the backend still locks, the renderer just isn't
/// told, which is why installing this is required rather than optional.
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

/// Wire the auto-lock event to the app handle. Called once during setup.
#[cfg(feature = "native-shell")]
pub fn install_auto_lock_sink(app: AppHandle) {
    set_event_sink(Arc::new(AppHandleSink(app)));
}
