//! System-tray module — the Tauri counterpart to `electron/src/tray.ts`.
//!
//! Behaviour mirrors the Electron build exactly:
//!   - Linux + Windows: tray is always created at startup (when the DE
//!     supports StatusNotifierItem / has a tray host). Unread state swaps
//!     the icon (`tray-default` ⇄ `tray-notification`) and tooltip. Closing
//!     the window hides it to the tray when "Close to tray" is on.
//!   - macOS: no tray by default — the dock badge already carries unread.
//!     The user opts in via the "Menu bar icon" preference, which calls
//!     `tray_set_enabled(true)`; unread does NOT swap the icon (it rides the
//!     dock badge, Slack/Linear style). The icon is a template image so it
//!     follows the light/dark menu-bar theme.
//!
//! The menu hosts Open / Mute mic / Version / Quit. The mute item reflects
//! the live call (pushed from the renderer via `tray_set_voice_state`) and,
//! when clicked, emits `tray:requestToggleMute` back to the renderer so the
//! voice session toggles its own mic — same contract as Electron.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{include_image, AppHandle, Emitter, Manager, Wry};

/// Tray runtime state, managed on the `AppHandle`. The icon handle is kept
/// alive here for its whole lifetime; dropping it removes the tray.
pub struct TrayState {
    tray: Mutex<Option<TrayIcon<Wry>>>,
    close_to_tray: AtomicBool,
    voice_in_call: AtomicBool,
    voice_muted: AtomicBool,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            tray: Mutex::new(None),
            // Matches the Electron default: hide-on-close is on unless the
            // user turns it off (and only ever applies when a tray exists).
            close_to_tray: AtomicBool::new(true),
            voice_in_call: AtomicBool::new(false),
            voice_muted: AtomicBool::new(false),
        }
    }
}

const DEFAULT_ICON: Image<'static> = include_image!("icons/tray-default.png");
const NOTIFICATION_ICON: Image<'static> = include_image!("icons/tray-notification.png");
const MAC_ICON: Image<'static> = include_image!("icons/tray-mac.png");

/// Bring the main window forward (restore + show + focus).
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Build the context menu from the current voice state. Rebuilt (rather than
/// mutated) on every voice transition — matches the Electron `rebuildMenu`.
fn build_menu(app: &AppHandle, in_call: bool, muted: bool) -> tauri::Result<Menu<Wry>> {
    let open = MenuItem::with_id(app, "open", "Open Pollis", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;

    let mute_label = if in_call {
        if muted {
            "Unmute mic"
        } else {
            "Mute mic"
        }
    } else {
        "Mute mic (not in a call)"
    };
    // The mute item is only enabled while in a call, so a stray click can't
    // toggle a mic that isn't publishing.
    let mute = MenuItem::with_id(app, "mute", mute_label, in_call, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let version = MenuItem::with_id(
        app,
        "version",
        format!("Version {}", app.package_info().version),
        false,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit Pollis", true, None::<&str>)?;

    Menu::with_items(
        app,
        &[
            &open as &dyn IsMenuItem<Wry>,
            &sep1,
            &mute,
            &sep2,
            &version,
            &quit,
        ],
    )
}

/// Create the tray icon if one doesn't already exist. Failure (e.g. a Linux
/// session with no StatusNotifierItem host) is logged, not propagated — the
/// app keeps running and close-to-tray falls back to a real close.
fn create_tray(app: &AppHandle) {
    let state = app.state::<TrayState>();
    if state.tray.lock().unwrap().is_some() {
        return;
    }

    let in_call = state.voice_in_call.load(Ordering::Relaxed);
    let muted = state.voice_muted.load(Ordering::Relaxed);
    let menu = match build_menu(app, in_call, muted) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("[tray] menu build failed: {err}");
            return;
        }
    };

    let is_mac = cfg!(target_os = "macos");
    let icon = if is_mac { MAC_ICON } else { DEFAULT_ICON };

    let builder = TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("Pollis")
        .menu(&menu)
        // macOS pops the menu on left-click (the user sees Open / Mute /
        // Quit). Linux/Windows reserve left-click for "bring window forward",
        // handled in on_tray_icon_event below; the menu is right-click there.
        .show_menu_on_left_click(is_mac)
        .icon_as_template(is_mac)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "mute" => {
                let _ = app.emit("tray:requestToggleMute", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // macOS already opens the menu on left-click; nothing to do.
            if cfg!(target_os = "macos") {
                return;
            }
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    match builder.build(app) {
        Ok(tray) => {
            *state.tray.lock().unwrap() = Some(tray);
        }
        Err(err) => {
            eprintln!("[tray] init failed — close-to-tray will be disabled: {err}");
        }
    }
}

/// Destroy the tray icon (macOS opt-out path).
fn destroy_tray(app: &AppHandle) {
    let state = app.state::<TrayState>();
    // Dropping the handle removes the icon; also remove by id defensively.
    let _ = app.remove_tray_by_id("main");
    *state.tray.lock().unwrap() = None;
}

/// Called once at startup. Linux/Windows create the tray immediately; macOS
/// waits for the renderer's "Menu bar icon" preference to enable it.
pub fn setup(app: &AppHandle) {
    if cfg!(target_os = "macos") {
        return;
    }
    create_tray(app);
}

/// Whether closing the window should hide-to-tray instead of quitting.
/// macOS never hides via this path (its dock-based close lives in lib.rs).
pub fn should_hide_on_close(app: &AppHandle) -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    let state = app.state::<TrayState>();
    state.close_to_tray.load(Ordering::Relaxed) && state.tray.lock().unwrap().is_some()
}

// ── Commands (invoked from the renderer via the bridge) ──────────────────────

/// Mirror the unread count into the tray icon + tooltip. No-op on macOS
/// (unread rides the dock badge there) and when no tray exists.
#[tauri::command]
pub fn tray_set_unread(app: AppHandle, count: i64) {
    let state = app.state::<TrayState>();
    let guard = state.tray.lock().unwrap();
    let Some(tray) = guard.as_ref() else {
        return;
    };

    #[cfg(not(target_os = "macos"))]
    {
        let icon = if count > 0 {
            NOTIFICATION_ICON
        } else {
            DEFAULT_ICON
        };
        let _ = tray.set_icon(Some(icon));
        let tooltip = if count > 0 {
            format!("Pollis — {count} unread")
        } else {
            "Pollis".to_string()
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }

    #[cfg(target_os = "macos")]
    {
        let _ = (tray, count);
    }
}

/// Toggle hide-to-tray-on-close (Linux/Windows). No-op on macOS.
#[tauri::command]
pub fn tray_set_close_to_tray(app: AppHandle, enabled: bool) {
    let state = app.state::<TrayState>();
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

/// Enable/disable the menu-bar tray (macOS only). Linux/Windows keep the
/// tray created at startup and ignore this.
#[tauri::command]
pub fn tray_set_enabled(app: AppHandle, enabled: bool) {
    if !cfg!(target_os = "macos") {
        return;
    }
    if enabled {
        create_tray(&app);
    } else {
        destroy_tray(&app);
    }
}

/// Push the live voice-call state so the tray's "Mute mic" item reflects the
/// real call. Rebuilds the menu only when something actually changed.
#[tauri::command]
pub fn tray_set_voice_state(app: AppHandle, in_call: bool, muted: bool) {
    let state = app.state::<TrayState>();
    let changed = state.voice_in_call.swap(in_call, Ordering::Relaxed) != in_call
        || state.voice_muted.swap(muted, Ordering::Relaxed) != muted;
    if !changed {
        return;
    }
    let guard = state.tray.lock().unwrap();
    if let Some(tray) = guard.as_ref() {
        if let Ok(menu) = build_menu(&app, in_call, muted) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}
