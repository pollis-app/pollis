// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::bots::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::bots::{BotDetail, BotHost, BotSummary};
pub use pollis_core::commands::bots::app_commands::GroupCommand;

#[tauri::command]
pub async fn list_group_bots(group_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<BotSummary>> {
    pollis_core::commands::bots::list_group_bots(&state, group_id).await
}

#[tauri::command]
pub async fn get_bot(bot_user_id: String, state: State<'_, Arc<AppState>>) -> Result<BotDetail> {
    pollis_core::commands::bots::get_bot(&state, bot_user_id).await
}

#[tauri::command]
pub async fn create_bot(
    group_id: String,
    code: String,
    display_name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<BotSummary> {
    pollis_core::commands::bots::create_bot(&state, group_id, code, display_name).await
}

#[tauri::command]
pub async fn add_bot_host(bot_user_id: String, code: String, state: State<'_, Arc<AppState>>) -> Result<BotHost> {
    pollis_core::commands::bots::add_bot_host(&state, bot_user_id, code).await
}

#[tauri::command]
pub async fn revoke_bot_host(bot_user_id: String, device_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::bots::revoke_bot_host(&state, bot_user_id, device_id).await
}

#[tauri::command]
pub async fn delete_bot(bot_user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::bots::delete_bot(&state, bot_user_id).await
}

// #2018: slash commands in the composer.
#[tauri::command]
pub async fn list_group_commands(group_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<GroupCommand>> {
    pollis_core::commands::bots::app_commands::list_group_commands(&state, group_id).await
}

#[tauri::command]
pub async fn send_command_request(
    group_id: String,
    channel_id: String,
    bot_user_id: String,
    command: String,
    args: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    pollis_core::commands::bots::app_commands::send_command_request(&state, group_id, channel_id, bot_user_id, command, args)
        .await
}
