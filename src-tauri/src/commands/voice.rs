// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::voice::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::voice::*;

#[tauri::command]
pub async fn subscribe_voice_events(on_event: tauri::ipc::Channel<pollis_core::commands::voice::VoiceEvent>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::subscribe_voice_events(std::sync::Arc::new(crate::sink::ChannelSink(on_event)), &state).await
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<Vec<AudioDevice>> {
    pollis_core::commands::voice::list_audio_devices().await
}

#[tauri::command]
pub async fn prepare_voice_connection(channel_id: String, user_id: String, display_name: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::prepare_voice_connection(channel_id, user_id, display_name, &state).await
}

#[tauri::command]
pub async fn join_voice_channel(channel_id: String, user_id: String, display_name: String, input_device: Option<String>, output_device: Option<String>, audio_processing: pollis_core::commands::voice_apm::ApmConfig, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::join_voice_channel(channel_id, user_id, display_name, input_device, output_device, audio_processing, &state).await
}

#[tauri::command]
pub async fn leave_voice_channel(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::leave_voice_channel(&state).await
}

#[tauri::command]
pub async fn toggle_voice_mute(state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::voice::toggle_voice_mute(&state).await
}

#[tauri::command]
pub async fn set_remote_user_volume(user_id: String, volume: f32, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::set_remote_user_volume(user_id, volume, &state).await
}

#[tauri::command]
pub async fn set_voice_input_device(device_name: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::set_voice_input_device(device_name, &state).await
}

#[tauri::command]
pub async fn set_voice_output_device(device_name: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::set_voice_output_device(device_name, &state).await
}

#[tauri::command]
pub async fn set_voice_audio_processing(config: pollis_core::commands::voice_apm::ApmConfig, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice::set_voice_audio_processing(config, &state).await
}

#[tauri::command]
pub async fn get_last_join_timings(state: State<'_, Arc<AppState>>) -> Result<Option<JoinTimings>> {
    pollis_core::commands::voice::get_last_join_timings(&state).await
}
