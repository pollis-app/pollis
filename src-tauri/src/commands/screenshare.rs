//! Generated-style shim. Forwards Tauri commands into
//! pollis_core::commands::screenshare. Edit pollis-core, not here.

use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::screenshare::*;

#[tauri::command]
pub async fn subscribe_screen_share_events(
    on_event: Channel<pollis_core::commands::screenshare::ScreenShareEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    pollis_core::commands::screenshare::subscribe_screen_share_events(
        Arc::new(crate::sink::ChannelSink(on_event)),
        &state,
    )
    .await
}

#[tauri::command]
pub async fn subscribe_screen_share_frames(
    on_frame: Channel<InvokeResponseBody>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    pollis_core::commands::screenshare::subscribe_screen_share_frames(
        Arc::new(crate::sink::RawChannelSink(on_frame)),
        &state,
    )
    .await
}

#[tauri::command]
pub async fn start_screen_share(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::screenshare::start_screen_share(&state).await
}

#[tauri::command]
pub async fn stop_screen_share(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::screenshare::stop_screen_share(&state).await
}
