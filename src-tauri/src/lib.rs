mod config;
pub mod db;
mod error;
mod keystore;
pub mod realtime;
mod signal;
mod state;
pub mod commands;

use std::sync::Arc;
use tauri::Manager;

use config::Config;
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Load .env.development in dev builds (no-op if file doesn't exist)
            #[cfg(debug_assertions)]
            let _ = dotenvy::from_filename(".env.development");

            let config = Config::from_env().map_err(|e| e.to_string())?;

            // Capture the window handle before app is moved into the async block.
            #[cfg(target_os = "linux")]
            let main_window = app.get_webview_window("main");

            tauri::async_runtime::block_on(async move {
                let state = AppState::new(config).await.map_err(|e| e.to_string())?;
                app.manage(Arc::new(state));
                Ok::<(), String>(())
            })?;

            // WebRTC is disabled by default in WebKitGTK and must be explicitly enabled.
            // Without this, RTCPeerConnection is undefined in the JS context on Linux.
            #[cfg(target_os = "linux")]
            if let Some(window) = main_window {
                use webkit2gtk::{SettingsExt, WebViewExt};
                let _ = window.with_webview(|webview| {
                    if let Some(settings) = webview.inner().settings() {
                        settings.set_enable_webrtc(true);
                        settings.set_enable_media_stream(true);
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::auth::initialize_identity,
            commands::auth::get_identity,
            commands::auth::request_otp,
            commands::auth::verify_otp,
            commands::auth::get_session,
            commands::auth::logout,
            commands::auth::delete_account,
            commands::user::get_user_profile,
            commands::user::update_user_profile,
            commands::user::search_user_by_username,
            commands::user::get_preferences,
            commands::user::save_preferences,
            commands::groups::list_user_groups,
            commands::groups::list_user_groups_with_channels,
            commands::groups::list_group_channels,
            commands::groups::create_group,
            commands::groups::create_channel,
            commands::groups::send_group_invite,
            commands::groups::get_pending_invites,
            commands::groups::accept_group_invite,
            commands::groups::decline_group_invite,
            commands::groups::request_group_access,
            commands::groups::get_group_join_requests,
            commands::groups::approve_join_request,
            commands::groups::reject_join_request,
            commands::groups::update_group,
            commands::groups::delete_group,
            commands::groups::get_group_members,
            commands::groups::remove_member_from_group,
            commands::groups::leave_group,
            commands::groups::update_channel,
            commands::groups::delete_channel,
            commands::groups::transfer_ownership,
            commands::groups::search_group_by_slug,
            commands::dm::create_dm_channel,
            commands::dm::list_dm_channels,
            commands::dm::get_dm_channel,
            commands::dm::add_user_to_dm_channel,
            commands::dm::remove_user_from_dm_channel,
            commands::dm::leave_dm_channel,
            commands::messages::list_messages,
            commands::messages::send_message,
            commands::messages::poll_pending_messages,
            commands::messages::get_channel_messages,
            commands::messages::get_dm_messages,
            commands::messages::list_messages_by_sender,
            commands::messages::list_channel_previews,
            commands::messages::search_messages,
            commands::messages::add_reaction,
            commands::messages::remove_reaction,
            commands::messages::get_reactions,
            commands::signal::get_prekey_bundle,
            commands::signal::rotate_signed_prekey,
            commands::signal::replenish_one_time_prekeys,
            commands::livekit::get_livekit_token,
            commands::livekit::get_livekit_url,
            commands::livekit::subscribe_realtime,
            commands::livekit::connect_rooms,
            commands::livekit::publish_ping,
            commands::r2::upload_file,
            commands::r2::download_file,
            commands::update::mark_update_required,
            commands::update::is_update_required,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pollis");
}
