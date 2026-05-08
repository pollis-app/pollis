// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::groups::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::groups::*;

#[tauri::command]
pub async fn list_user_groups(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<Group>> {
    pollis_core::commands::groups::list_user_groups(user_id, &state).await
}

#[tauri::command]
pub async fn list_user_groups_with_channels(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<GroupWithChannels>> {
    pollis_core::commands::groups::list_user_groups_with_channels(user_id, &state).await
}

#[tauri::command]
pub async fn list_group_channels(group_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<Channel>> {
    pollis_core::commands::groups::list_group_channels(group_id, &state).await
}

#[tauri::command]
pub async fn create_group(name: String, description: Option<String>, owner_id: String, create_default_text_channel: Option<bool>, create_default_voice_channel: Option<bool>, state: State<'_, Arc<AppState>>) -> Result<Group> {
    pollis_core::commands::groups::create_group(name, description, owner_id, create_default_text_channel, create_default_voice_channel, &state).await
}

#[tauri::command]
pub async fn create_channel(group_id: String, name: String, description: Option<String>, channel_type: Option<String>, _creator_id: String, state: State<'_, Arc<AppState>>) -> Result<Channel> {
    pollis_core::commands::groups::create_channel(group_id, name, description, channel_type, _creator_id, &state).await
}

#[tauri::command]
pub async fn send_group_invite(group_id: String, inviter_id: String, invitee_identifier: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::send_group_invite(group_id, inviter_id, invitee_identifier, &state).await
}

#[tauri::command]
pub async fn get_pending_invites(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<PendingInvite>> {
    pollis_core::commands::groups::get_pending_invites(user_id, &state).await
}

#[tauri::command]
pub async fn accept_group_invite(invite_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::accept_group_invite(invite_id, user_id, &state).await
}

#[tauri::command]
pub async fn decline_group_invite(invite_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::decline_group_invite(invite_id, user_id, &state).await
}

#[tauri::command]
pub async fn request_group_access(group_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::request_group_access(group_id, requester_id, &state).await
}

#[tauri::command]
pub async fn get_group_join_requests(group_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<JoinRequest>> {
    pollis_core::commands::groups::get_group_join_requests(group_id, requester_id, &state).await
}

#[tauri::command]
pub async fn get_my_join_request(group_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<Option<JoinRequest>> {
    pollis_core::commands::groups::get_my_join_request(group_id, requester_id, &state).await
}

#[tauri::command]
pub async fn approve_join_request(request_id: String, approver_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::approve_join_request(request_id, approver_id, &state).await
}

#[tauri::command]
pub async fn reject_join_request(request_id: String, approver_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::reject_join_request(request_id, approver_id, &state).await
}

#[tauri::command]
pub async fn update_group(group_id: String, requester_id: String, name: Option<String>, description: Option<String>, icon_url: Option<String>, state: State<'_, Arc<AppState>>) -> Result<Group> {
    pollis_core::commands::groups::update_group(group_id, requester_id, name, description, icon_url, &state).await
}

#[tauri::command]
pub async fn delete_group(group_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::delete_group(group_id, requester_id, &state).await
}

#[tauri::command]
pub async fn get_group_members(group_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<GroupMember>> {
    pollis_core::commands::groups::get_group_members(group_id, &state).await
}

#[tauri::command]
pub async fn remove_member_from_group(group_id: String, user_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::remove_member_from_group(group_id, user_id, requester_id, &state).await
}

#[tauri::command]
pub async fn leave_group(group_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::leave_group(group_id, user_id, &state).await
}

#[tauri::command]
pub async fn update_channel(channel_id: String, requester_id: String, name: Option<String>, description: Option<String>, state: State<'_, Arc<AppState>>) -> Result<Channel> {
    pollis_core::commands::groups::update_channel(channel_id, requester_id, name, description, &state).await
}

#[tauri::command]
pub async fn delete_channel(channel_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::delete_channel(channel_id, requester_id, &state).await
}

#[tauri::command]
pub async fn set_member_role(group_id: String, user_id: String, role: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::groups::set_member_role(group_id, user_id, role, requester_id, &state).await
}

#[tauri::command]
pub async fn search_group_by_slug(slug: String, state: State<'_, Arc<AppState>>) -> Result<Group> {
    pollis_core::commands::groups::search_group_by_slug(slug, &state).await
}
