// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::messages::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::messages::*;

#[tauri::command]
pub async fn list_messages(conversation_id: String, limit: Option<i64>, before_id: Option<String>, state: State<'_, Arc<AppState>>) -> Result<Vec<Message>> {
    pollis_core::commands::messages::list_messages(conversation_id, limit, before_id, &state).await
}

#[tauri::command]
pub async fn send_message(conversation_id: String, sender_id: String, content: String, reply_to_id: Option<String>, thread_id: Option<String>, sender_username: Option<String>, state: State<'_, Arc<AppState>>) -> Result<Message> {
    pollis_core::commands::messages::send_message(conversation_id, sender_id, content, reply_to_id, thread_id, sender_username, &state).await
}

#[tauri::command]
pub async fn get_channel_messages(user_id: String, channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>, state: State<'_, Arc<AppState>>) -> Result<MessagePage> {
    pollis_core::commands::messages::get_channel_messages(user_id, channel_id, limit, cursor, &state).await
}

#[tauri::command]
pub async fn get_dm_messages(user_id: String, dm_channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>, state: State<'_, Arc<AppState>>) -> Result<MessagePage> {
    pollis_core::commands::messages::get_dm_messages(user_id, dm_channel_id, limit, cursor, &state).await
}

#[tauri::command]
pub async fn read_channel_messages(channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>, state: State<'_, Arc<AppState>>) -> Result<MessagePage> {
    pollis_core::commands::messages::read_channel_messages(channel_id, limit, cursor, &state).await
}

#[tauri::command]
pub async fn read_dm_messages(dm_channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>, state: State<'_, Arc<AppState>>) -> Result<MessagePage> {
    pollis_core::commands::messages::read_dm_messages(dm_channel_id, limit, cursor, &state).await
}

#[tauri::command]
pub async fn read_last_messages(conversation_ids: Vec<String>, state: State<'_, Arc<AppState>>) -> Result<Vec<ChannelMessage>> {
    pollis_core::commands::messages::read_last_messages(conversation_ids, &state).await
}

#[tauri::command]
pub async fn read_thread_messages(thread_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<ChannelMessage>> {
    pollis_core::commands::messages::read_thread_messages(thread_id, &state).await
}

#[tauri::command]
pub async fn list_thread_summaries(conversation_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<ThreadSummary>> {
    pollis_core::commands::messages::list_thread_summaries(conversation_id, &state).await
}

#[tauri::command]
pub async fn ingest_channel_envelopes(user_id: String, channel_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::ingest_channel_envelopes(user_id, channel_id, &state).await
}

#[tauri::command]
pub async fn ingest_dm_envelopes(user_id: String, dm_channel_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::ingest_dm_envelopes(user_id, dm_channel_id, &state).await
}

#[tauri::command]
pub async fn search_messages(query: String, limit: Option<i64>, state: State<'_, Arc<AppState>>) -> Result<Vec<SearchResult>> {
    pollis_core::commands::messages::search_messages(query, limit, &state).await
}

#[tauri::command]
pub async fn add_reaction(message_id: String, user_id: String, emoji: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::add_reaction(message_id, user_id, emoji, &state).await
}

#[tauri::command]
pub async fn remove_reaction(message_id: String, user_id: String, emoji: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::remove_reaction(message_id, user_id, emoji, &state).await
}

#[tauri::command]
pub async fn get_reactions(message_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<Reaction>> {
    pollis_core::commands::messages::get_reactions(message_id, &state).await
}

#[tauri::command]
pub async fn mark_messages_read(conversation_id: String, user_id: String, message_ids: Vec<String>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::mark_messages_read(conversation_id, user_id, message_ids, &state).await
}

#[tauri::command]
pub async fn get_unread_counts(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<UnreadCount>> {
    pollis_core::commands::messages::get_unread_counts(user_id, &state).await
}

#[tauri::command]
pub async fn mark_conversation_read(conversation_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::messages::mark_conversation_read(conversation_id, user_id, &state).await
}

#[tauri::command]
pub async fn get_conversation_receipts(conversation_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<MessageReceipts>> {
    pollis_core::commands::messages::get_conversation_receipts(conversation_id, &state).await
}

#[tauri::command]
pub async fn delete_message(message_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::delete_message(message_id, user_id, &state).await
}

#[tauri::command]
pub async fn edit_message(conversation_id: String, message_id: String, user_id: String, new_content: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::edit_message(conversation_id, message_id, user_id, new_content, &state).await
}

#[tauri::command]
pub async fn get_message_retention(state: State<'_, Arc<AppState>>) -> Result<i64> {
    pollis_core::commands::messages::get_message_retention(&state).await
}

#[tauri::command]
pub async fn set_message_retention(days: i64, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::messages::set_message_retention(days, &state).await
}

#[tauri::command]
pub async fn run_message_eviction(state: State<'_, Arc<AppState>>) -> Result<usize> {
    pollis_core::commands::messages::run_message_eviction(&state).await
}
