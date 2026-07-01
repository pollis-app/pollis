//! Generated-style shim. Forwards Tauri commands into
//! pollis_core::commands::camera. Edit pollis-core, not here.

use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::camera::*;

#[tauri::command]
pub async fn subscribe_camera_events(
    on_event: Channel<pollis_core::commands::camera::CameraEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    pollis_core::commands::camera::subscribe_camera_events(
        Arc::new(crate::sink::ChannelSink(on_event)),
        &state,
    )
    .await
}

#[tauri::command]
pub async fn list_video_devices(
    state: State<'_, Arc<AppState>>,
) -> Result<pollis_capture_proto::CameraList> {
    pollis_core::commands::camera::list_video_devices(&state).await
}

#[tauri::command]
pub async fn start_camera(state: State<'_, Arc<AppState>>, device_id: String) -> Result<()> {
    pollis_core::commands::camera::start_camera(&state, device_id).await
}

#[tauri::command]
pub async fn stop_camera(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::camera::stop_camera(&state).await
}
