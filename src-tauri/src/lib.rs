pub use pollis_core::accounts;
pub use pollis_core::config;
pub use pollis_core::error;
pub use pollis_core::keystore;
pub use pollis_core::state;
pub mod sink;
pub mod commands;
pub mod pathscope;
#[cfg(feature = "native-shell")]
pub mod tray;

#[cfg(feature = "test-harness")]
pub mod test_harness;

// Gated so the headless lib builds warning-free.
#[cfg(feature = "native-shell")]
use std::sync::Arc;
#[cfg(feature = "native-shell")]
use tauri::Manager;

#[cfg(feature = "native-shell")]
use config::Config;
#[cfg(feature = "native-shell")]
use state::AppState;

/// Tear down every live capture (screen share, camera, preview); idempotent.
#[cfg(feature = "native-shell")]
async fn stop_all_capture(state: &Arc<AppState>) {
    let _ = pollis_core::commands::screenshare::stop_screen_share(state).await;
    let _ = pollis_core::commands::camera::stop_camera(state).await;
    let _ = pollis_core::commands::camera::stop_camera_preview(state).await;
}

/// macOS: hide the window on close instead of destroying it.
#[cfg(all(feature = "native-shell", target_os = "macos"))]
fn hide_on_close(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        // Cmd+W only hides, so stop an active screen-share or it captures forever.
        if let Some(state) = window.app_handle().try_state::<Arc<AppState>>() {
            let state = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                stop_all_capture(&state).await;
            });
        }
        api.prevent_close();
        let _ = window.hide();
    }
}

/// Rounded corners via public AppKit APIs; re-adds the titled masks so the traffic lights exist despite `decorations: false`.
#[cfg(all(feature = "native-shell", target_os = "macos"))]
fn apply_macos_rounded_corners(window: &tauri::WebviewWindow, radius: f64) {
    use cocoa::appkit::{NSWindow, NSWindowStyleMask, NSWindowTitleVisibility};
    use cocoa::base::{id, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};

    let ns_window = match window.ns_window() {
        Ok(w) => w as id,
        Err(_) => return,
    };
    unsafe {
        let mut mask = ns_window.styleMask();
        mask |= NSWindowStyleMask::NSFullSizeContentViewWindowMask
            | NSWindowStyleMask::NSTitledWindowMask
            | NSWindowStyleMask::NSClosableWindowMask
            | NSWindowStyleMask::NSMiniaturizableWindowMask
            | NSWindowStyleMask::NSResizableWindowMask;
        ns_window.setStyleMask_(mask);
        ns_window.setTitlebarAppearsTransparent_(YES);
        ns_window.setTitleVisibility_(NSWindowTitleVisibility::NSWindowTitleHidden);
        let _: () = msg_send![ns_window, setOpaque: NO];
        let clear: id = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![ns_window, setBackgroundColor: clear];
        let _: () = msg_send![ns_window, setHasShadow: YES];

        let content_view: id = msg_send![ns_window, contentView];
        let _: () = msg_send![content_view, setWantsLayer: YES];
        let layer: id = msg_send![content_view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: YES];
        }
    }
}

#[cfg(all(feature = "native-shell", target_os = "windows"))]
fn apply_windows_rounded_corners(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    if let Ok(hwnd) = window.hwnd() {
        let hwnd = hwnd.0 as HWND;
        let pref: u32 = DWMWCP_ROUND as u32;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &pref as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

#[cfg(all(feature = "native-shell", target_os = "macos"))]
fn show_on_reopen(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Clipboard file paths. macOS Finder uses `public.file-url`, which `read_text()` can't see, hence `osascript`.
#[cfg(feature = "native-shell")]
#[tauri::command]
fn read_clipboard_files(app: tauri::AppHandle) -> Vec<String> {
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

    #[cfg(not(target_os = "macos"))]
    {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let Ok(text) = app.clipboard().read_text() else {
            return vec![];
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

/// Clipboard image as PNG bytes, returned rather than written to temp so it never touches disk.
#[cfg(feature = "native-shell")]
#[tauri::command]
async fn read_clipboard_image(app: tauri::AppHandle) -> tauri::ipc::Response {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let empty = || tauri::ipc::Response::new(Vec::<u8>::new());

    let Ok(image) = app.clipboard().read_image() else {
        return empty();
    };

    let width = image.width();
    let height = image.height();
    let rgba = image.rgba().to_vec();

    let Some(buffer) = image::RgbaImage::from_raw(width, height, rgba) else {
        return empty();
    };

    let mut png = std::io::Cursor::new(Vec::new());
    if buffer
        .write_to(&mut png, image::ImageFormat::Png)
        .is_err()
    {
        return empty();
    }

    tauri::ipc::Response::new(png.into_inner())
}

/// Via the clipboard plugin: `navigator.clipboard` is unreliable on WebKitGTK (#854).
#[cfg(feature = "native-shell")]
#[tauri::command]
fn write_clipboard_text(app: tauri::AppHandle, text: String) -> bool {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    app.clipboard().write_text(text).is_ok()
}

/// Cmd+W: hide on macOS, close elsewhere.
#[cfg(feature = "native-shell")]
#[tauri::command]
fn hide_window(window: tauri::Window) {
    #[cfg(target_os = "macos")]
    let _ = window.hide();

    #[cfg(not(target_os = "macos"))]
    let _ = window.close();
}

#[cfg(feature = "native-shell")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // macOS GUI apps get a soft RLIMIT_NOFILE of 256; EMFILE breaks CoreAudio, DNS and websockets.
    #[cfg(unix)]
    unsafe {
        let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 {
            let target = rl.rlim_max.min(65_536);
            if rl.rlim_cur < target {
                let prev = rl.rlim_cur;
                rl.rlim_cur = target;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &rl) == 0 {
                    pollis_core::diag!("[startup] raised RLIMIT_NOFILE soft limit {prev} -> {target}");
                } else {
                    pollis_core::diag!("[startup] setrlimit(RLIMIT_NOFILE, {target}) failed: {}", std::io::Error::last_os_error());
                }
            }
        }
    }

    // WebKitGTK DMA-BUF rendering aborts without GBM/EGL; honour POLLIS_ENABLE_COMPOSITING like main.rs.
    #[cfg(target_os = "linux")]
    if std::env::var("POLLIS_ENABLE_COMPOSITING").is_err() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // A missing `autoaudiosink` crashes WebKitWebProcess; default to `pulsesink` unless the user set one.
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
            #[cfg(debug_assertions)]
            let _ = dotenvy::from_filename(".env.development");

            let mut config = Config::from_env().map_err(|e| e.to_string())?;
            // #1611: which build this is, for the minimum-version check and the DS's 426.
            config.client = Some(pollis_core::client_policy::ClientIdentity::desktop());

            let tray_handle = app.handle().clone();

            #[cfg(target_os = "linux")]
            let main_window = app.get_webview_window("main");

            // Radius matches `border-radius: 10px` in index.css.
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                apply_macos_rounded_corners(&window, 10.0);
            }

            #[cfg(target_os = "macos")]
            tauri::async_runtime::spawn_blocking(commands::install_kind::heal_self_quarantine_at_startup);

            // decorations: false needs an explicit DWM opt-in for rounding (Windows 11 only).
            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                apply_windows_rounded_corners(&window);
            }

            if let Ok(data_dir) = app.path().app_data_dir() {
                let cache_dir = data_dir.join("media-cache");
                let _ = pollis_core::private_fs::create_dir_all(&cache_dir);
                pollis_core::commands::r2::set_media_cache_dir(cache_dir);
            }

            let log_dir = pollis_core::diag::log_dir(&pollis_core::db::local::dirs_path());
            if let Err(e) = pollis_core::diag::init(&log_dir) {
                pollis_core::diag!("[diag] could not open the diagnostics log: {e}");
            }

            tauri::async_runtime::block_on(async move {
                let state = AppState::new(config).await.map_err(|e| e.to_string())?;
                let state = Arc::new(state);

                // A failure must not block startup; it leaves the overlay off (direct).
                let boot_mode = state.config.overlay_mode;
                if let Err(e) =
                    pollis_core::commands::overlay::apply_overlay_mode(&state, boot_mode).await
                {
                    pollis_core::diag!("[overlay] boot apply ({boot_mode:?}) failed, staying direct: {e}");
                }

                // Spawned before `manage` so the port is set before any frontend code runs.
                match pollis_core::media_server::spawn(state.clone()).await {
                    Ok(port) => {
                        *state.media_server_port.lock().await = Some(port);
                    }
                    Err(e) => {
                        pollis_core::diag!("[setup] failed to spawn media server: {e}");
                    }
                }

                // Without this a consenting device runs a node nothing can reach (#813).
                pollis_core::commands::relay_serving::attach_app_state(&state);

                app.manage(state);
                Ok::<(), String>(())
            })?;

            tray_handle.manage(tray::TrayState::default());
            tray::setup(&tray_handle);

            // Pushed as an event because polling is banned.
            commands::relay_serving::install_status_sink(tray_handle.clone());

            // Rust owns the deadline: a hidden WebView throttles its timers (#851).
            commands::autolock::install_auto_lock_sink(tray_handle.clone());

            // Written only by Rust-driven pickers and DragDrop, so only a human grows it.
            tray_handle.manage(crate::pathscope::PathScope::default());

            tray_handle.manage(commands::media_permissions::MediaPermissionsState::default());

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
            read_clipboard_image,
            write_clipboard_text,
            tray::tray_set_unread,
            tray::tray_set_close_to_tray,
            tray::tray_set_enabled,
            tray::tray_set_voice_state,
            commands::media_permissions::get_media_permission_status,
            commands::media_permissions::open_privacy_settings,
            commands::media_permissions::revoke_media_permissions,
            commands::media_permissions::set_revoke_media_on_exit,
            commands::auth::initialize_identity,
            commands::diagnostics::get_diagnostics,
            commands::auth::request_otp,
            commands::auth::verify_otp,
            commands::auth::request_email_change_otp,
            commands::auth::verify_email_change,
            commands::auth::dev_login,
            commands::auth::get_session,
            commands::auth::get_device_id,
            commands::auth::logout,
            commands::auth::delete_account,
            commands::auth::list_known_accounts,
            commands::auth::wipe_local_data,
            commands::pin::set_pin,
            commands::pin::unlock,
            commands::pin::lock,
            commands::pin::get_unlock_state,
            commands::autolock::set_auto_lock_timeout,
            commands::autolock::report_user_activity,
            commands::auth::list_user_devices,
            commands::auth::revoke_device,
            commands::auth::is_current_device_registered,
            commands::auth::is_current_device_revoked,
            commands::device_enrollment::start_device_enrollment,
            commands::device_enrollment::poll_enrollment_status,
            commands::device_enrollment::await_enrollment_approval,
            commands::device_enrollment::list_pending_enrollment_requests,
            commands::device_enrollment::approve_device_enrollment,
            commands::device_enrollment::reject_device_enrollment,
            commands::device_enrollment::recover_with_secret_key,
            commands::device_enrollment::reset_identity_and_recover,
            commands::device_enrollment::finalize_device_enrollment,
            commands::device_link::create_device_link,
            commands::device_link::poll_device_link,
            commands::device_link::await_device_link,
            commands::device_link::approve_device_link,
            commands::device_link::cancel_device_link,
            commands::device_link::claim_device_link,
            commands::device_link::create_device_offer,
            commands::device_link::await_device_offer,
            commands::device_link::cancel_device_offer,
            commands::device_link::accept_device_offer,
            commands::device_link::finish_device_offer,
            commands::device_enrollment::list_security_events,
            commands::safety::get_safety_number,
            commands::safety::set_contact_verified,
            commands::safety::list_peer_verifications,
            commands::transparency::self_audit_account_key,
            commands::transparency::audit_peer_account_key,
            commands::transparency::verify_own_build,
            commands::overlay::get_overlay_mode,
            commands::overlay::set_overlay_mode,
            commands::relay_serving::get_relay_serving_status,
            commands::relay_serving::set_relay_serving,
            commands::user::get_user_profile,
            commands::user::update_user_profile,
            commands::user::search_user_by_username,
            commands::user::get_preferences,
            commands::user::save_preferences,
            commands::groups::list_user_groups,
            commands::groups::list_user_groups_with_channels,
            commands::groups::list_group_channels,
            commands::groups::list_muted_channels,
            commands::groups::set_channel_muted,
            commands::groups::create_group,
            commands::groups::create_channel,
            commands::groups::send_group_invite,
            commands::groups::get_pending_invites,
            commands::groups::accept_group_invite,
            commands::groups::decline_group_invite,
            commands::groups::list_group_invites,
            commands::groups::revoke_group_invite,
            commands::groups::create_group_invite_link,
            commands::groups::list_group_invite_links,
            commands::groups::revoke_group_invite_link,
            commands::groups::redeem_group_invite_link,
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
            commands::dm::list_dm_requests,
            commands::dm::accept_dm_request,
            commands::dm::get_dm_channel,
            commands::dm::add_user_to_dm_channel,
            commands::dm::remove_user_from_dm_channel,
            commands::dm::leave_dm_channel,
            commands::blocks::block_user,
            commands::blocks::unblock_user,
            commands::blocks::report_user,
            commands::blocks::list_blocked_users,
            // Saved messages + permalinks (#854); device-local only.
            commands::bookmarks::save_message,
            commands::bookmarks::unsave_message,
            commands::bookmarks::toggle_saved_message,
            commands::bookmarks::list_saved_messages,
            commands::bookmarks::resolve_message_permalink,
            // On-device export (#856).
            commands::export::export_archive,
            commands::export::fetch_export_attachments,
            commands::pinned_messages::pin_message,
            commands::pinned_messages::unpin_message,
            commands::pinned_messages::list_pinned_messages,
            commands::vault::get_vault_messages,
            commands::vault::send_vault_message,
            commands::vault::edit_vault_message,
            commands::vault::set_vault_message_pinned,
            commands::vault::delete_vault_message,
            commands::vault::search_vault_messages,
            commands::messages::list_messages,
            commands::messages::send_message,
            commands::messages::new_message_id,
            commands::messages::retry_message,
            commands::messages::discard_unsent_message,
            commands::messages::flush_outbox,
            commands::messages::get_channel_messages,
            commands::messages::get_dm_messages,
            commands::messages::read_channel_messages,
            commands::messages::read_dm_messages,
            commands::messages::read_last_messages,
            commands::messages::read_thread_messages,
            commands::messages::list_thread_summaries,
            commands::messages::ingest_channel_envelopes,
            commands::messages::ingest_dm_envelopes,
            commands::messages::search_messages,
            commands::messages::rebuild_search_index,
            commands::messages::read_messages_around,
            commands::messages::read_messages_after,
            commands::messages::add_reaction,
            commands::messages::remove_reaction,
            commands::messages::get_reactions,
            commands::messages::mark_messages_read,
            commands::messages::get_unread_counts,
            commands::messages::mark_conversation_read,
            commands::messages::sync_read_cursors,
            commands::messages::get_conversation_receipts,
            commands::messages::delete_message,
            commands::messages::edit_message,
            commands::messages::get_message_retention,
            commands::messages::set_message_retention,
            commands::messages::run_message_eviction,
            commands::mls::poll_mls_welcomes,
            commands::mls::process_pending_commits,
            commands::mls::catch_up_all_mls_groups,
            commands::livekit::get_livekit_token,
            commands::livekit::get_livekit_view_token,
            commands::livekit::get_livekit_url,
            commands::livekit::subscribe_realtime,
            commands::livekit::connect_rooms,
            commands::livekit::publish_ping,
            commands::livekit::publish_typing,
            commands::livekit::publish_voice_presence,
            commands::livekit::list_voice_participants,
            commands::livekit::list_voice_room_counts,
            commands::livekit::start_call,
            commands::livekit::cancel_call,
            commands::emoji::list_usable_emoji,
            commands::emoji::list_group_emoji,
            commands::emoji::upload_group_emoji,
            commands::emoji::remove_group_emoji,
            commands::emoji::get_emoji_url,
            commands::emoji::prepare_emoji_text,
            commands::r2::upload_file,
            commands::r2::upload_public_file,
            commands::r2::get_public_file_url,
            commands::r2::save_media_to_path,
            commands::r2::upload_media,
            commands::r2::upload_media_staged,
            commands::staging::stage_attachment,
            commands::staging::discard_staged_attachment,
            commands::r2::download_file,
            commands::r2::download_media,
            commands::r2::get_media_url,
            commands::update::mark_update_required,
            commands::update::is_update_required,
            commands::update::get_update_check_plan,
            commands::client_policy::check_client_policy,
            commands::client_policy::get_client_policy_status,
            commands::install_kind::detect_managed_install,
            commands::install_kind::detect_read_only_location,
            commands::voice::subscribe_voice_events,
            commands::voice::list_audio_devices,
            commands::voice::prepare_voice_connection,
            commands::voice::join_voice_channel,
            commands::voice::leave_voice_channel,
            commands::voice::toggle_voice_mute,
            commands::voice::toggle_voice_deafen,
            commands::voice::set_voice_input_mode,
            commands::voice::set_voice_ptt_held,
            commands::voice::release_voice_ptt,
            commands::voice::get_voice_gate_state,
            commands::voice::set_remote_user_volume,
            commands::voice::set_voice_input_device,
            commands::voice::set_voice_output_device,
            commands::voice::set_voice_audio_processing,
            commands::voice::get_last_join_timings,
            commands::voice_test::subscribe_voice_test_events,
            commands::voice_test::start_mic_test,
            commands::voice_test::set_mic_test_monitor,
            commands::voice_test::stop_mic_test,
            commands::voice_test::record_and_play_back,
            commands::voice_test::play_test_tone,
            commands::voice_test::stop_test_playback,
            commands::screenshare::subscribe_screen_share_events,
            commands::screenshare::subscribe_screen_share_frames,
            commands::screenshare::screenshare_ws_url,
            commands::screenshare::enumerate_screen_sources,
            commands::screenshare::cancel_screen_share_picker,
            commands::screenshare::start_screen_share,
            commands::screenshare::stop_screen_share,
            commands::camera::subscribe_camera_events,
            commands::camera::list_video_devices,
            commands::camera::start_camera,
            commands::camera::stop_camera,
            commands::camera::start_camera_preview,
            commands::camera::stop_camera_preview,
            commands::sfx::play_sfx,
            commands::sfx::start_ring,
            commands::sfx::stop_ring,
            commands::pathscope::pick_open_paths,
            commands::pathscope::pick_save_path,
            commands::terminal::get_terminal_enabled,
            commands::terminal::set_terminal_enabled,
            commands::terminal::terminal_open,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close,
            commands::terminal::terminal_ack,
        ])
        // No media-cache sweep on focus (#930): the cap is enforced on cache writes.
        .on_window_event(|_window, _event| {
            #[cfg(target_os = "macos")]
            hide_on_close(_window, _event);
            // Paths come from the OS event, not the renderer, so only a human grows `pathscope`.
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = _event {
                let app = _window.app_handle();
                if let Some(scope) = app.try_state::<crate::pathscope::PathScope>() {
                    crate::pathscope::remember_dropped(&scope, paths);
                }
                // The fs plugin keeps its own allowlist; the preview reads through it.
                if let Some(fs) = tauri_plugin_fs::FsExt::try_fs_scope(app) {
                    for path in paths {
                        let _ = if path.is_dir() {
                            fs.allow_directory(path, true)
                        } else {
                            fs.allow_file(path)
                        };
                    }
                }
            }
            if let tauri::WindowEvent::Focused(true) = _event {
                // Evict messages past the local retention window.
                if let Some(state) = _window.app_handle().try_state::<Arc<AppState>>() {
                    let state = state.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        let _ = pollis_core::commands::messages::run_message_eviction(&state).await;
                    });
                }
            }
            // Only tear down the share on a real close; hiding to tray keeps the app running.
            #[cfg(not(target_os = "macos"))]
            if let tauri::WindowEvent::CloseRequested { api, .. } = _event {
                if crate::tray::should_hide_on_close(_window.app_handle()) {
                    api.prevent_close();
                    let _ = _window.hide();
                } else if let Some(state) =
                    _window.app_handle().try_state::<Arc<AppState>>()
                {
                    let state = state.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        stop_all_capture(&state).await;
                    });
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Pollis")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_on_reopen(_app);
            }
            // Wipe the whole media cache on exit; readers treat a missing file as a miss.
            if let tauri::RunEvent::ExitRequested { .. } = _event {
                pollis_core::commands::r2::clear_media_cache(
                    pollis_core::commands::r2::CacheScope::Everything,
                );
                // Tauri does not guarantee state drop order before _exit, so kill the helper here.
                if let Some(state) = _app.try_state::<Arc<AppState>>() {
                    let state = state.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        stop_all_capture(&state).await;
                        // Close LiveKit rooms so we don't linger as a voice ghost; nothing else calls shutdown.
                        state.shutdown().await;
                    });
                }
                // After capture teardown, so a grant is never cleared under a live capture.
                if let Some(mp) =
                    _app.try_state::<commands::media_permissions::MediaPermissionsState>()
                {
                    commands::media_permissions::revoke_on_exit_if_enabled(_app, mp.inner());
                }
            }
        });
}
