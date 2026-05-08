// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::livekit::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::livekit::*;

#[tauri::command]
pub async fn get_livekit_token(room_name: String, identity: String, display_name: String, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::livekit::get_livekit_token(room_name, identity, display_name, &state).await
}

#[tauri::command]
pub async fn get_livekit_url(state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::livekit::get_livekit_url(&state).await
}

#[tauri::command]
pub async fn subscribe_realtime(on_event: tauri::ipc::Channel<pollis_core::realtime::RealtimeEvent>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::livekit::subscribe_realtime(std::sync::Arc::new(crate::sink::ChannelSink(on_event)), &state).await
}

#[tauri::command]
pub async fn connect_rooms(room_ids: Vec<String>, user_id: String, username: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::livekit::connect_rooms(room_ids, user_id, username, &state).await
}

#[tauri::command]
pub async fn publish_ping(room_id: String, channel_id: Option<String>, conversation_id: Option<String>, sender_id: String, sender_username: Option<String>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::livekit::publish_ping(room_id, channel_id, conversation_id, sender_id, sender_username, &state).await
}

#[tauri::command]
pub async fn publish_voice_presence(group_id: String, channel_id: String, user_id: String, display_name: String, joined: bool, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::livekit::publish_voice_presence(group_id, channel_id, user_id, display_name, joined, &state).await
}

#[tauri::command]
pub async fn list_voice_participants(channel_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<VoiceParticipantInfo>> {
    pollis_core::commands::livekit::list_voice_participants(channel_id, &state).await
}

#[tauri::command]
pub async fn list_voice_room_counts(channel_ids: Vec<String>, state: State<'_, Arc<AppState>>) -> Result<Vec<VoiceRoomCount>> {
    pollis_core::commands::livekit::list_voice_room_counts(channel_ids, &state).await
}

#[tauri::command]
pub async fn start_call(callee_id: String, caller_id: String, caller_username: String, state: State<'_, Arc<AppState>>) -> Result<StartCallResult> {
    pollis_core::commands::livekit::start_call(callee_id, caller_id, caller_username, &state).await
}

#[tauri::command]
pub async fn cancel_call(other_user_id: String, call_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::livekit::cancel_call(other_user_id, call_id, &state).await
}
