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

/// Read a raster image from the OS clipboard and return it as PNG bytes.
/// Empty when the clipboard holds no image data.
///
/// Used as a fallback for clipboard content that the WebKit paste event
/// doesn't expose as `DataTransferItem` files — notably screenshots and
/// images copied from a browser on Linux. macOS WebKit surfaces these as
/// JS File objects directly, so this is mainly a Linux/Windows path.
///
/// It used to save the PNG into the OS temp directory as
/// `pollis-paste-<nanos>.png` and hand back the path. Nothing ever deleted it,
/// so every screenshot a user had ever pasted stayed in `/tmp` in the clear.
/// Returning the bytes means the renderer can build its preview and hand them
/// straight to `stage_attachment`, and the image never touches a disk at all —
/// see `pollis_core::commands::staging`.
#[cfg(feature = "native-shell")]
#[tauri::command]
async fn read_clipboard_image(app: tauri::AppHandle) -> tauri::ipc::Response {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let empty = || tauri::ipc::Response::new(Vec::<u8>::new());

    let image = match app.clipboard().read_image() {
        Ok(img) => img,
        Err(_) => return empty(),
    };

    let width = image.width();
    let height = image.height();
    let rgba = image.rgba().to_vec();

    let buffer = match image::RgbaImage::from_raw(width, height, rgba) {
        Some(buf) => buf,
        None => return empty(),
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

/// Write plain text to the OS clipboard.
///
/// Used by "copy link" (#854). Goes through the clipboard-manager plugin rather
/// than `navigator.clipboard` because the latter is unreliable on WebKitGTK,
/// which is the Linux webview this app ships on.
#[cfg(feature = "native-shell")]
#[tauri::command]
fn write_clipboard_text(app: tauri::AppHandle, text: String) -> bool {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    app.clipboard().write_text(text).is_ok()
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
    // DRM). Disable it so the app doesn't crash on launch.
    //
    // NOT unconditional, despite what this comment used to say. `main.rs`
    // deliberately skips both WebKit disables when POLLIS_ENABLE_COMPOSITING is
    // set, and this line then set one of them straight back — so the documented
    // escape hatch could not actually be exercised, and nobody could measure
    // the accelerated path even by asking for it. Honouring the same opt-in
    // here is what makes that flag mean something.
    //
    // The perf stake is real: with WebKitGTK's accelerated compositing off
    // there is no WebGL, so the in-app terminal falls back to a slower renderer
    // (see TerminalView.tsx). Launching at all still wins over launching fast,
    // which is why the conservative path remains the default.
    #[cfg(target_os = "linux")]
    if std::env::var("POLLIS_ENABLE_COMPOSITING").is_err() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

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
                let _ = pollis_core::private_fs::create_dir_all(&cache_dir);
                pollis_core::commands::r2::set_media_cache_dir(cache_dir);
            }

            tauri::async_runtime::block_on(async move {
                let state = AppState::new(config).await.map_err(|e| e.to_string())?;
                let state = Arc::new(state);

                // Honor POLLIS_OVERLAY at boot through the runtime apply path
                // (design §14: construct DBs direct, then apply the mode). A
                // failure here must not block startup — it leaves the overlay off
                // (direct), the safe default.
                let boot_mode = state.config.overlay_mode;
                if let Err(e) =
                    pollis_core::commands::overlay::apply_overlay_mode(&state, boot_mode).await
                {
                    eprintln!("[overlay] boot apply ({boot_mode:?}) failed, staying direct: {e}");
                }

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

                // Hand the relay-serving manager the signed directory + device
                // identity it needs to park outbound connections (#813). Without
                // this a consenting device runs its node but nothing can reach
                // it, and the status line says exactly that.
                pollis_core::commands::relay_serving::attach_app_state(&state);

                app.manage(state);
                Ok::<(), String>(())
            })?;

            // System tray (Linux/Windows created now; macOS opt-in via the
            // renderer's "Menu bar icon" preference → tray_set_enabled).
            tray_handle.manage(tray::TrayState::default());
            tray::setup(&tray_handle);

            // Push relay-serving status changes to the renderer as a global
            // event. Required rather than optional: CLAUDE.md bans polling, so
            // without this the "Run a relay for others" status line would only
            // refresh when Preferences remounts.
            commands::relay_serving::install_status_sink(tray_handle.clone());

            // Push the idle auto-lock to the renderer as a global event (#851).
            // The deadline is owned by Rust because a WebView throttles its own
            // timers exactly when the window is hidden — the case auto-lock
            // exists for.
            commands::autolock::install_auto_lock_sink(tray_handle.clone());

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
            commands::autolock::set_auto_lock_timeout,
            commands::autolock::report_user_activity,
            commands::auth::list_user_devices,
            commands::auth::revoke_device,
            commands::auth::is_current_device_registered,
            commands::device_enrollment::start_device_enrollment,
            commands::device_enrollment::poll_enrollment_status,
            commands::device_enrollment::await_enrollment_approval,
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
            commands::groups::create_group,
            commands::groups::create_channel,
            commands::groups::send_group_invite,
            commands::groups::get_pending_invites,
            commands::groups::accept_group_invite,
            commands::groups::decline_group_invite,
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
            commands::blocks::list_blocked_users,
            // Saved messages + permalinks (#854). Device-local; no DS endpoint
            // backs any of these, and resolve_message_permalink deliberately
            // only ever consults the local database.
            commands::bookmarks::save_message,
            commands::bookmarks::unsave_message,
            commands::bookmarks::toggle_saved_message,
            commands::bookmarks::list_saved_messages,
            commands::bookmarks::resolve_message_permalink,
            // Pinned messages (#99)
            commands::pinned_messages::pin_message,
            commands::pinned_messages::unpin_message,
            commands::pinned_messages::list_pinned_messages,
            // Vault (#107)
            commands::vault::get_vault_messages,
            commands::vault::send_vault_message,
            commands::vault::edit_vault_message,
            commands::vault::set_vault_message_pinned,
            commands::vault::delete_vault_message,
            commands::vault::search_vault_messages,
            commands::messages::list_messages,
            commands::messages::send_message,
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
            commands::r2::upload_media,
            commands::r2::upload_media_staged,
            commands::staging::stage_attachment,
            commands::staging::discard_staged_attachment,
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
            commands::terminal::terminal_open,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close,
            commands::terminal::terminal_ack,
        ])
        // On macOS, hide the window on close instead of quitting.
        //
        // The media-cache cap is deliberately NOT re-evaluated here (#930).
        // It used to be: every alt-tab walked the whole cache directory to
        // total its size, which scales with months of accumulated media rather
        // than with anything the user just did, and after #874 removed the
        // seven focus-time IPC calls it was the only work left on focus. The
        // cap is enforced by `pollis_core::commands::r2` on every cache write
        // instead — a cache nobody is writing to cannot grow past it.
        .on_window_event(|_window, _event| {
            #[cfg(target_os = "macos")]
            hide_on_close(_window, _event);
            if let tauri::WindowEvent::Focused(true) = _event {
                // Bounded local history: evict messages past the device-local
                // retention window on focus. This one is bounded by the
                // retention window, not by the size of anything on disk.
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
            // Wipe the media cache on app exit. Its files are AES-GCM
            // encrypted under the session's `db_key`, but exit is a lifecycle
            // boundary and media the user viewed has no reason to outlive the
            // process that fetched it.
            //
            // `Everything`, not one user: this is the process going away, so
            // there is no session left to keep a cache warm for. A second
            // instance signed in as somebody else simply re-fetches — every
            // read path treats a missing cache file as a cache miss.
            if let tauri::RunEvent::ExitRequested { .. } = _event {
                pollis_core::commands::r2::clear_media_cache(
                    pollis_core::commands::r2::CacheScope::Everything,
                );
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
                        // ghost for everyone still in the channel. Tauri fires
                        // no teardown hook of its own, so this is the only place
                        // `AppState::shutdown` gets called — call it explicitly.
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
