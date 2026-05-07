// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::voice_test::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::voice_test::*;

#[tauri::command]
pub async fn subscribe_voice_test_events(on_event: tauri::ipc::Channel<pollis_core::commands::voice_test::VoiceTestEvent>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::subscribe_voice_test_events(std::sync::Arc::new(crate::sink::ChannelSink(on_event)), &state).await
}

#[tauri::command]
pub async fn start_mic_test(input_device_id: String, output_device_id: String, monitor: bool, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::start_mic_test(input_device_id, output_device_id, monitor, &state).await
}

#[tauri::command]
pub async fn set_mic_test_monitor(enabled: bool, output_device_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::set_mic_test_monitor(enabled, output_device_id, &state).await
}

#[tauri::command]
pub async fn stop_mic_test(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::stop_mic_test(&state).await
}

#[tauri::command]
pub async fn record_and_play_back(input_device_id: String, output_device_id: String, duration_ms: u32, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::record_and_play_back(input_device_id, output_device_id, duration_ms, &state).await
}

#[tauri::command]
pub async fn play_test_tone(output_device_id: String, kind: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::play_test_tone(output_device_id, kind, &state).await
}

#[tauri::command]
pub async fn stop_test_playback(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::voice_test::stop_test_playback(&state).await
}
