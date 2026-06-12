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
pub async fn screenshare_ws_url(state: State<'_, Arc<AppState>>) -> Result<Option<String>> {
    Ok(pollis_core::commands::screenshare::screenshare_ws_url(&state).await?)
}

#[tauri::command]
pub async fn enumerate_screen_sources(
    state: State<'_, Arc<AppState>>,
) -> Result<pollis_capture_proto::SourceList> {
    pollis_core::commands::screenshare::enumerate_screen_sources(&state).await
}

#[tauri::command]
pub async fn cancel_screen_share_picker(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::screenshare::cancel_screen_share_picker(&state).await
}

#[tauri::command]
pub async fn start_screen_share(
    state: State<'_, Arc<AppState>>,
    selection: Option<pollis_capture_proto::Selection>,
) -> Result<()> {
    pollis_core::commands::screenshare::start_screen_share(&state, selection).await
}

#[tauri::command]
pub async fn stop_screen_share(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::screenshare::stop_screen_share(&state).await
}
