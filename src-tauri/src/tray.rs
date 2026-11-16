//! System tray. macOS has none unless opted in; the renderer stays the single owner of mic state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{include_image, AppHandle, Emitter, Manager, Wry};

/// Holds the icon handle alive; dropping it removes the tray.
pub struct TrayState {
    tray: Mutex<Option<TrayIcon<Wry>>>,
    close_to_tray: AtomicBool,
    voice_in_call: AtomicBool,
    voice_muted: AtomicBool,
    labels: Mutex<Option<TrayLabels>>,
}

/// The menu's copy, translated by the renderer (`common:tray.*`, #1724), which owns the language
/// choice. English until the renderer first sends it.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    open: String,
    mute_mic: String,
    unmute_mic: String,
    mute_mic_idle: String,
    version: String,
    quit: String,
}

impl TrayLabels {
    fn english(version: &str) -> Self {
        Self {
            open: "Open Pollis".into(),
            mute_mic: "Mute mic".into(),
            unmute_mic: "Unmute mic".into(),
            mute_mic_idle: "Mute mic (not in a call)".into(),
            version: format!("Version {version}"),
            quit: "Quit Pollis".into(),
        }
    }
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            tray: Mutex::new(None),
            // Default from the pre-#386 Electron shell, so upgraders see no change.
            close_to_tray: AtomicBool::new(true),
            voice_in_call: AtomicBool::new(false),
            voice_muted: AtomicBool::new(false),
            labels: Mutex::new(None),
        }
    }
}

const DEFAULT_ICON: Image<'static> = include_image!("icons/tray-default.png");
const NOTIFICATION_ICON: Image<'static> = include_image!("icons/tray-notification.png");
const MAC_ICON: Image<'static> = include_image!("icons/tray-mac.png");

/// Shows, unminimises and focuses the main window (it may be hidden to the tray).
pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Rebuilt, not mutated, on every voice transition.
fn build_menu(app: &AppHandle, in_call: bool, muted: bool) -> tauri::Result<Menu<Wry>> {
    let labels = app
        .state::<TrayState>()
        .labels
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| TrayLabels::english(&app.package_info().version.to_string()));
    let open = MenuItem::with_id(app, "open", &labels.open, true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;

    let mute_label = if in_call {
        if muted {
            &labels.unmute_mic
        } else {
            &labels.mute_mic
        }
    } else {
        &labels.mute_mic_idle
    };
    // Only enabled in a call, so a stray click can't toggle an idle mic.
    let mute = MenuItem::with_id(app, "mute", mute_label, in_call, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let version = MenuItem::with_id(app, "version", &labels.version, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", &labels.quit, true, None::<&str>)?;

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

/// Failure (no StatusNotifierItem host) is logged; close-to-tray falls back to a real close.
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
            pollis_core::diag!("[tray] menu build failed: {err}");
            return;
        }
    };

    let is_mac = cfg!(target_os = "macos");
    let icon = if is_mac { MAC_ICON } else { DEFAULT_ICON };

    let builder = TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("Pollis")
        .menu(&menu)
        // Linux/Windows reserve left-click for "bring window forward".
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
            pollis_core::diag!("[tray] init failed — close-to-tray will be disabled: {err}");
        }
    }
}

fn destroy_tray(app: &AppHandle) {
    let state = app.state::<TrayState>();
    let _ = app.remove_tray_by_id("main");
    *state.tray.lock().unwrap() = None;
}

pub fn setup(app: &AppHandle) {
    if cfg!(target_os = "macos") {
        return;
    }
    create_tray(app);
}

/// macOS never hides via this path (see lib.rs).
pub fn should_hide_on_close(app: &AppHandle) -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    let state = app.state::<TrayState>();
    state.close_to_tray.load(Ordering::Relaxed) && state.tray.lock().unwrap().is_some()
}

/// Mirror unread into the tray icon + tooltip. No-op on macOS. `tooltip` is the renderer's
/// translated "Pollis — N unread" (#1724); without one the tooltip is English.
#[tauri::command]
pub fn tray_set_unread(app: AppHandle, count: i64, tooltip: Option<String>) {
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
            tooltip.unwrap_or_else(|| format!("Pollis — {count} unread"))
        } else {
            "Pollis".to_string()
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }

    #[cfg(target_os = "macos")]
    {
        let _ = (tray, count, tooltip);
    }
}

#[tauri::command]
pub fn tray_set_close_to_tray(app: AppHandle, enabled: bool) {
    let state = app.state::<TrayState>();
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

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

/// Swap in a menu built from the current voice state and labels, if the tray exists.
fn rebuild_menu(app: &AppHandle) {
    let state = app.state::<TrayState>();
    let in_call = state.voice_in_call.load(Ordering::Relaxed);
    let muted = state.voice_muted.load(Ordering::Relaxed);
    let guard = state.tray.lock().unwrap();
    if let Some(tray) = guard.as_ref() {
        if let Ok(menu) = build_menu(app, in_call, muted) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

/// Rebuilds the menu only when something changed.
#[tauri::command]
pub fn tray_set_voice_state(app: AppHandle, in_call: bool, muted: bool) {
    let state = app.state::<TrayState>();
    let changed = state.voice_in_call.swap(in_call, Ordering::Relaxed) != in_call
        || state.voice_muted.swap(muted, Ordering::Relaxed) != muted;
    if changed {
        rebuild_menu(&app);
    }
}

/// The renderer's translation of the menu (#1724): stored, and the menu rebuilt if the tray exists.
#[tauri::command]
pub fn tray_set_labels(app: AppHandle, labels: TrayLabels) {
    *app.state::<TrayState>().labels.lock().unwrap() = Some(labels);
    rebuild_menu(&app);
}
