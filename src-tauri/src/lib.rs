mod accounts;
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

/// On macOS, intercept the window close request (Cmd+W / red traffic light)
/// and hide the window instead of destroying it. The app keeps running in
/// the dock and can be re-opened without a cold start.
#[cfg(target_os = "macos")]
fn hide_on_close(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        // Prevent the window from actually being destroyed.
        api.prevent_close();
        // Hide the window — it can be shown again from the dock.
        let _ = window.hide();
    }
}

/// On macOS, re-show the main window when the user clicks the dock icon
/// (RunEvent::Reopen).
#[cfg(target_os = "macos")]
fn show_on_reopen(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Read the OS clipboard and return any file paths found in it.
///
/// On macOS, Finder puts file references on the clipboard using
/// `public.file-url` (NSPasteboard), not as plain text with file:// URIs.
/// The clipboard-manager plugin's `read_text()` can't see those, so we
/// shell out to `osascript` to read NSPasteboard file URLs directly.
///
/// On Linux, file managers use the text/uri-list MIME type with file:// URIs,
/// which `read_text()` picks up fine.
#[tauri::command]
fn read_clipboard_files(app: tauri::AppHandle) -> Vec<String> {
    // macOS: read file URLs from NSPasteboard via AppleScript-ObjC bridge
    #[cfg(target_os = "macos")]
    {
        let _ = &app; // suppress unused warning
        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(concat!(
                "use framework \"AppKit\"\n",
                "set pb to current application's NSPasteboard's generalPasteboard()\n",
                "set urls to pb's readObjectsForClasses:{current application's NSURL} options:(missing value)\n",
                "if urls is missing value then return \"\"\n",
                "set paths to {}\n",
                "repeat with u in urls\n",
                "if (u's isFileURL()) as boolean then\n",
                "set end of paths to (u's |path|()) as text\n",
                "end if\n",
                "end repeat\n",
                "set AppleScript's text item delimiters to linefeed\n",
                "return paths as text",
            ))
            .output();

        return match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect()
            }
            Err(_) => vec![],
        };
    }

    // Linux/Windows: read text clipboard for file:// URIs (text/uri-list)
    #[cfg(not(target_os = "macos"))]
    {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let text = match app.clipboard().read_text() {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        text.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|line| {
                let url = url::Url::parse(line).ok()?;
                if url.scheme() != "file" { return None; }
                let path = url.to_file_path().ok()?;
                Some(path.to_string_lossy().into_owned())
            })
            .collect()
    }
}

/// Cmd+W handler: hide the window on macOS (matching hide_on_close behaviour)
/// or close it on Windows/Linux.
#[tauri::command]
fn hide_window(window: tauri::Window) {
    #[cfg(target_os = "macos")]
    let _ = window.hide();

    #[cfg(not(target_os = "macos"))]
    let _ = window.close();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK 2.42+ attempts DMA-BUF rendering and aborts if GBM/EGL is
    // unavailable (e.g. certain GPU drivers, VMs, Wayland compositors without
    // DRM). Disable it unconditionally so the app doesn't crash on launch.
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");

    // WebKit uses GStreamer for media playback. The `autoaudiosink` element
    // (gst-plugins-good) is not always installed. When it is missing, GStreamer
    // returns NULL and WebKitWebProcess crashes with a GLib NULL-pointer assertion
    // instead of degrading gracefully. Setting GST_AUDIO_SINK to `pulsesink`
    // (provided by gst-plugins-good on PulseAudio/PipeWire systems) is safer;
    // on PipeWire the PulseAudio compatibility layer handles it transparently.
    // We only override if the user hasn't set it themselves.
    #[cfg(target_os = "linux")]
    if std::env::var("GST_AUDIO_SINK").is_err() {
        std::env::set_var("GST_AUDIO_SINK", "pulsesink");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
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
            hide_window,
            read_clipboard_files,
            commands::auth::initialize_identity,
            commands::auth::get_identity,
            commands::auth::request_otp,
            commands::auth::verify_otp,
            commands::auth::dev_login,
            commands::auth::get_session,
            commands::auth::logout,
            commands::auth::delete_account,
            commands::auth::list_known_accounts,
            commands::auth::list_user_devices,
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
            commands::groups::get_my_join_request,
            commands::groups::approve_join_request,
            commands::groups::reject_join_request,
            commands::groups::update_group,
            commands::groups::delete_group,
            commands::groups::get_group_members,
            commands::groups::remove_member_from_group,
            commands::groups::leave_group,
            commands::groups::update_channel,
            commands::groups::delete_channel,
            commands::groups::set_member_role,
            commands::groups::search_group_by_slug,
            commands::dm::create_dm_channel,
            commands::dm::list_dm_channels,
            commands::dm::get_dm_channel,
            commands::dm::add_user_to_dm_channel,
            commands::dm::remove_user_from_dm_channel,
            commands::dm::leave_dm_channel,
            commands::messages::list_messages,
            commands::messages::send_message,
            commands::messages::get_channel_messages,
            commands::messages::get_dm_messages,
            commands::messages::list_messages_by_sender,
            commands::messages::list_channel_previews,
            commands::messages::search_messages,
            commands::messages::add_reaction,
            commands::messages::remove_reaction,
            commands::messages::get_reactions,
            commands::messages::delete_message,
            commands::messages::edit_message,
            commands::mls::generate_mls_key_package,
            commands::mls::publish_mls_key_package,
            commands::mls::fetch_mls_key_package,
            commands::mls::create_mls_group,
            commands::mls::process_welcome,
            commands::mls::poll_mls_welcomes,
            commands::mls::add_member_mls,
            commands::mls::remove_member_mls,
            commands::mls::process_pending_commits,
commands::livekit::get_livekit_token,
            commands::livekit::get_livekit_url,
            commands::livekit::subscribe_realtime,
            commands::livekit::connect_rooms,
            commands::livekit::publish_ping,
            commands::livekit::publish_voice_presence,
            commands::livekit::list_voice_participants,
            commands::livekit::list_voice_room_counts,
            commands::r2::upload_file,
            commands::r2::upload_media,
            commands::r2::download_file,
            commands::r2::download_media,
            commands::update::mark_update_required,
            commands::update::is_update_required,
            commands::voice::subscribe_voice_events,
            commands::voice::list_audio_devices,
            commands::voice::join_voice_channel,
            commands::voice::leave_voice_channel,
            commands::voice::toggle_voice_mute,
            commands::voice::set_voice_input_device,
            commands::voice::set_voice_output_device,
            commands::voice::set_noise_floor,
            commands::sfx::play_sfx,
        ])
        // On macOS, hide the window on close instead of quitting.
        .on_window_event(|_window, _event| {
            #[cfg(target_os = "macos")]
            hide_on_close(_window, _event);
        })
        .build(tauri::generate_context!())
        .expect("error while building Pollis")
        .run(|app, event| {
            // On macOS, re-show the window when the dock icon is clicked.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_on_reopen(app);
            }
        });
}
