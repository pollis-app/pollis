pub use pollis_core::accounts;
pub use pollis_core::config;
pub use pollis_core::db;
pub use pollis_core::error;
pub use pollis_core::keystore;
pub use pollis_core::realtime;
pub use pollis_core::signal;
pub use pollis_core::sink as core_sink;
pub use pollis_core::state;
pub mod sink;
pub mod commands;
// The system tray is built from Wry-typed handles (TrayIcon<Wry>, Menu<Wry>);
// only compiled with the native shell.
#[cfg(feature = "native-shell")]
pub mod tray;

#[cfg(feature = "test-harness")]
pub mod test_harness;

// These imports are only used by the native-shell helpers + run() below, all
// gated on `native-shell`. Gate the imports too so the headless lib builds
// warning-free (the test harness pulls in AppState via its own `use`).
#[cfg(feature = "native-shell")]
use std::sync::Arc;
#[cfg(feature = "native-shell")]
use tauri::Manager;

#[cfg(feature = "native-shell")]
use config::Config;
#[cfg(feature = "native-shell")]
use state::AppState;

/// On macOS, intercept the window close request (Cmd+W / red traffic light)
/// and hide the window instead of destroying it. The app keeps running in
/// the dock and can be re-opened without a cold start.
#[cfg(all(feature = "native-shell", target_os = "macos"))]
fn hide_on_close(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        // Cmd+W only hides the window (real quit is Cmd+Q →
        // ExitRequested), so an active screen-share would otherwise keep
        // capturing forever with no way to stop it. Tear it down before
        // hiding. stop_screen_share is idempotent — a no-op when nothing
        // is sharing.
        if let Some(state) = window.app_handle().try_state::<Arc<AppState>>() {
            let state = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                let _ = pollis_core::commands::screenshare::stop_screen_share(&state).await;
                let _ = pollis_core::commands::camera::stop_camera(&state).await;
                let _ = pollis_core::commands::camera::stop_camera_preview(&state).await;
            });
        }
        // Prevent the window from actually being destroyed.
        api.prevent_close();
        // Hide the window — it can be shown again from the dock.
        let _ = window.hide();
    }
}

/// Apply rounded corners to an NSWindow using only public AppKit APIs.
/// Technique: make the window non-opaque with a clear background, then set
/// the contentView's CALayer cornerRadius + masksToBounds so the rendered
/// content is clipped to a rounded rect.
///
/// Titlebar: we keep the macOS "hidden inset" style — a transparent,
/// title-hidden titlebar with the native traffic lights visible, sitting over
/// the full-size content view. This matches the Electron build
/// (`titleBarStyle: "hidden"`) and the frontend `TitleBar`, which reserves a
/// 68px slot at top-left for the native controls. The window config sets
/// `decorations: false` (borderless, no buttons), so we re-add the
/// titled/closable/miniaturizable/resizable masks here to bring the traffic
/// lights back; without them the reserved 68px slot renders empty.
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
        // Merge in FullSizeContentView so the webview paints under the
        // titlebar region, and restore the titled/closable/miniaturizable/
        // resizable masks so the native traffic lights exist (decorations:false
        // strips them). The titlebar itself stays transparent + title-hidden,
        // so only the three buttons show over the rounded content below.
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

/// On macOS, re-show the main window when the user clicks the dock icon
/// (RunEvent::Reopen).
#[cfg(all(feature = "native-shell", target_os = "macos"))]
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
#[cfg(feature = "native-shell")]
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

/// Read a raster image from the OS clipboard, encode it as PNG, and write
/// it to a temporary file. Returns the absolute path, or an empty string
/// if the clipboard does not contain image data.
///
/// Used as a fallback for clipboard content that the WebKit paste event
/// doesn't expose as `DataTransferItem` files — notably screenshots and
/// images copied from a browser on Linux. macOS WebKit surfaces these as
/// JS File objects directly, so this is mainly a Linux/Windows path.
#[cfg(feature = "native-shell")]
#[tauri::command]
async fn read_clipboard_image_to_temp(app: tauri::AppHandle) -> String {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let image = match app.clipboard().read_image() {
        Ok(img) => img,
        Err(_) => return String::new(),
    };

    let width = image.width();
    let height = image.height();
    let rgba = image.rgba().to_vec();

    let buffer = match image::RgbaImage::from_raw(width, height, rgba) {
        Some(buf) => buf,
        None => return String::new(),
    };

    let path = std::env::temp_dir().join(format!(
        "pollis-paste-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    if buffer.save(&path).is_err() {
        return String::new();
    }

    path.to_string_lossy().into_owned()
}

/// Cmd+W handler: hide the window on macOS (matching hide_on_close behaviour)
/// or close it on Windows/Linux.
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
    // macOS launches GUI apps with a soft `RLIMIT_NOFILE` of 256, which is
    // not enough once realtime LiveKit rooms (one per group), libsql
    // websockets, reqwest connection pools, the local media-cache HTTP
    // server, and CoreAudio AudioUnits all coexist. Hitting the cap surfaces
    // as `EMFILE` (`Too many open files`) inside CoreAudio (`UpdateStreamFormats:
    // 0 output streams`), `libsystem_dnssd` (`socketpair failed 24`), and
    // websocket reconnects — i.e. voice silently failing to publish, devices
    // disappearing from enumeration, and random kicks from voice channels.
    // Raise the soft limit to the hard max; the hard max on macOS is
    // typically `unlimited` (OPEN_MAX, 10240 in practice).
    #[cfg(unix)]
    unsafe {
        let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 {
            let target = rl.rlim_max.min(65_536);
            if rl.rlim_cur < target {
                let prev = rl.rlim_cur;
                rl.rlim_cur = target;
                if libc::setrlimit(libc::RLIMIT_NOFILE, &rl) == 0 {
                    eprintln!("[startup] raised RLIMIT_NOFILE soft limit {prev} -> {target}");
                } else {
                    eprintln!("[startup] setrlimit(RLIMIT_NOFILE, {target}) failed: {}", std::io::Error::last_os_error());
                }
            }
        }
    }

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

            // Owned app handle captured before `app` is moved into the async
            // block below — used to set up the system tray afterwards.
            let tray_handle = app.handle().clone();

            // Capture the window handle before app is moved into the async block.
            #[cfg(target_os = "linux")]
            let main_window = app.get_webview_window("main");

            // Round the window corners using public APIs only (App Store
            // compliant). We set the contentView's CALayer cornerRadius and
            // mask it, then make the NSWindow non-opaque with a clear
            // background so the area outside the rounded rect isn't drawn.
            // Radius matches `border-radius: 10px` in index.css.
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                apply_macos_rounded_corners(&window, 10.0);
            }

            // Windows 11 rounds decorated windows via DWM. Since we use
            // decorations: false we must opt in explicitly. Windows 10 has
            // no API for this and falls back to square corners.
            #[cfg(target_os = "windows")]
            if let Some(window) = app.get_webview_window("main") {
                apply_windows_rounded_corners(&window);
            }

            // Initialise the on-disk media cache. The frontend renders
            // attachments by file path (via convertFileSrc) instead of
            // pumping decrypted bytes through the JSON IPC.
            if let Ok(data_dir) = app.path().app_data_dir() {
                let cache_dir = data_dir.join("media-cache");
                let _ = std::fs::create_dir_all(&cache_dir);
                pollis_core::commands::r2::set_media_cache_dir(cache_dir);
            }

            tauri::async_runtime::block_on(async move {
                let state = AppState::new(config).await.map_err(|e| e.to_string())?;
                let state = Arc::new(state);

                // Loopback HTTP server for cached media. The webview
                // embeds `http://127.0.0.1:<port>/<token>/<hash>` URLs
                // for every `<img>/<audio>/<video>` element. Spawned
                // before `manage` so the port is on `AppState` by the
                // time any frontend code runs.
                match pollis_core::media_server::spawn(state.clone()).await {
                    Ok(port) => {
                        *state.media_server_port.lock().await = Some(port);
                    }
                    Err(e) => {
                        eprintln!("[setup] failed to spawn media server: {e}");
                    }
                }

                app.manage(state);
                Ok::<(), String>(())
            })?;

            // System tray (Linux/Windows created now; macOS opt-in via the
            // renderer's "Menu bar icon" preference → tray_set_enabled).
            tray_handle.manage(tray::TrayState::default());
            tray::setup(&tray_handle);

            // Holds the "revoke media permissions on quit" preference so the
            // ExitRequested hook can read it synchronously at shutdown.
            tray_handle.manage(commands::media_permissions::MediaPermissionsState::default());

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
            read_clipboard_image_to_temp,
            tray::tray_set_unread,
            tray::tray_set_close_to_tray,
            tray::tray_set_enabled,
            tray::tray_set_voice_state,
            commands::media_permissions::get_media_permission_status,
            commands::media_permissions::revoke_media_permissions,
            commands::media_permissions::set_revoke_media_on_exit,
            commands::auth::initialize_identity,
            commands::auth::get_identity,
            commands::auth::request_otp,
            commands::auth::verify_otp,
            commands::auth::request_email_change_otp,
            commands::auth::verify_email_change,
            commands::auth::dev_login,
            commands::auth::get_session,
            commands::auth::logout,
            commands::auth::delete_account,
            commands::auth::list_known_accounts,
            commands::auth::wipe_local_data,
            commands::pin::set_pin,
            commands::pin::unlock,
            commands::pin::lock,
            commands::pin::get_unlock_state,
            commands::auth::list_user_devices,
            commands::auth::revoke_device,
            commands::device_enrollment::start_device_enrollment,
            commands::device_enrollment::poll_enrollment_status,
            commands::device_enrollment::list_pending_enrollment_requests,
            commands::device_enrollment::approve_device_enrollment,
            commands::device_enrollment::reject_device_enrollment,
            commands::device_enrollment::recover_with_secret_key,
            commands::device_enrollment::reset_identity_and_recover,
            commands::device_enrollment::finalize_device_enrollment,
            commands::device_enrollment::list_security_events,
            commands::safety::get_safety_number,
            commands::safety::set_contact_verified,
            commands::safety::list_peer_verifications,
            commands::transparency::self_audit_account_key,
            commands::transparency::audit_peer_account_key,
            commands::transparency::verify_own_build,
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
            commands::dm::list_dm_requests,
            commands::dm::accept_dm_request,
            commands::dm::get_dm_channel,
            commands::dm::add_user_to_dm_channel,
            commands::dm::remove_user_from_dm_channel,
            commands::dm::leave_dm_channel,
            commands::blocks::block_user,
            commands::blocks::unblock_user,
            commands::blocks::list_blocked_users,
            commands::messages::list_messages,
            commands::messages::send_message,
            commands::messages::get_channel_messages,
            commands::messages::get_dm_messages,
            commands::messages::read_channel_messages,
            commands::messages::read_dm_messages,
            commands::messages::ingest_channel_envelopes,
            commands::messages::ingest_dm_envelopes,
            commands::messages::list_messages_by_sender,
            commands::messages::list_channel_previews,
            commands::messages::search_messages,
            commands::messages::add_reaction,
            commands::messages::remove_reaction,
            commands::messages::get_reactions,
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
            commands::r2::upload_file,
            commands::r2::upload_media,
            commands::r2::download_file,
            commands::r2::download_media,
            commands::r2::get_media_url,
            commands::update::mark_update_required,
            commands::update::is_update_required,
            commands::install_kind::detect_managed_install,
            commands::voice::subscribe_voice_events,
            commands::voice::list_audio_devices,
            commands::voice::prepare_voice_connection,
            commands::voice::join_voice_channel,
            commands::voice::leave_voice_channel,
            commands::voice::toggle_voice_mute,
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
            commands::terminal::terminal_open,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close,
            commands::terminal::terminal_ack,
        ])
        // On macOS, hide the window on close instead of quitting.
        // On window focus, re-evaluate the media-cache cap so files
        // copied into the dir externally / mtime-tampered / etc. don't
        // let it grow past the limit.
        .on_window_event(|_window, _event| {
            #[cfg(target_os = "macos")]
            hide_on_close(_window, _event);
            if let tauri::WindowEvent::Focused(true) = _event {
                pollis_core::commands::r2::enforce_cache_cap_now();
                // Bounded local history: evict messages past the device-local
                // retention window on focus, alongside the media-cache cap.
                if let Some(state) = _window.app_handle().try_state::<Arc<AppState>>() {
                    let state = state.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        let _ = pollis_core::commands::messages::run_message_eviction(&state).await;
                    });
                }
            }
            // On Linux/Windows closing the window either hides it to the
            // tray (when "Close to tray" is on and a tray exists) or really
            // quits. When it hides, the app keeps running — so we must NOT
            // tear down the screen-share. When it really closes, Tauri runs
            // CloseRequested before ExitRequested gets a chance, and the
            // helper child can briefly outlive the parent (PR_SET_PDEATHSIG
            // catches it eventually but the portal screencast indicator /
            // red dot lingers). Stop the share synchronously in that case so
            // the user sees an immediate clean shutdown.
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
                        let _ = pollis_core::commands::screenshare::stop_screen_share(&state).await;
                        let _ = pollis_core::commands::camera::stop_camera(&state).await;
                        let _ = pollis_core::commands::camera::stop_camera_preview(&state).await;
                    });
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Pollis")
        .run(|_app, _event| {
            // On macOS, re-show the window when the dock icon is clicked.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_on_reopen(_app);
            }
            // Wipe the plaintext media cache on app exit. The cache holds
            // decrypted bytes (image / video / audio) and is not encrypted
            // at rest, so it must not survive a graceful shutdown — the
            // next attacker with file-system access would otherwise be
            // able to read every media file the user viewed.
            if let tauri::RunEvent::ExitRequested { .. } = _event {
                pollis_core::commands::r2::clear_media_cache();
                // Also kill any active screen-share helper subprocess.
                // kill_on_drop on the Child handle would normally take
                // care of this when AppState is dropped, but Tauri does
                // not guarantee state drop ordering before _exit, so we
                // explicitly nuke it here. Belt-and-suspenders: the
                // helper also installs PR_SET_PDEATHSIG=SIGTERM so a
                // hard parent crash still cleans it up.
                if let Some(state) = _app.try_state::<Arc<AppState>>() {
                    let state = state.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        let _ = pollis_core::commands::screenshare::stop_screen_share(&state).await;
                        let _ = pollis_core::commands::camera::stop_camera(&state).await;
                        let _ = pollis_core::commands::camera::stop_camera_preview(&state).await;
                        // Close the LiveKit rooms (realtime + voice) so the
                        // server evicts us immediately instead of waiting out its
                        // RTC timeout — otherwise our voice card lingers as a
                        // ghost for everyone still in the channel. The Electron
                        // path gets this via pollis-node's before-quit shutdown;
                        // Tauri has no equivalent, so call it explicitly here.
                        state.shutdown().await;
                    });
                }
                // If the user opted into "revoke system permissions when Pollis
                // quits", best-effort clear the macOS TCC grants now. Reads the
                // atomic synchronously — no async prefs fetch at exit. No-op on
                // Linux/Windows. Runs after the capture teardown above so we
                // never clear a grant out from under a still-live capture.
                if let Some(mp) =
                    _app.try_state::<commands::media_permissions::MediaPermissionsState>()
                {
                    commands::media_permissions::revoke_on_exit_if_enabled(_app, mp.inner());
                }
            }
        });
}
