//! Media permissions — OS-level camera / microphone / screen-share access.
//!
//! Unlike the other command modules this one is NOT a thin shim over
//! `pollis-core`: querying and clearing OS privacy grants is inherently a
//! shell-runtime concern (TCC on macOS, the ConsentStore registry on Windows,
//! the `ms-settings:` deep-links, the app bundle identifier), so it lives
//! entirely in `src-tauri` — the same rationale as `install_kind.rs` / `tray.rs`.
//!
//! Honest, per-OS behaviour:
//!   - **macOS** — live status via `AVCaptureDevice::authorizationStatusForMediaType`
//!     (camera + mic) and `CGPreflightScreenCaptureAccess()` (screen). "Revoke
//!     now" spawns `tccutil reset <Service> <bundle-id>`, which *clears* the
//!     saved grant so macOS re-prompts on next use (it does NOT set a permanent
//!     deny).
//!   - **Linux** — there is no TCC-equivalent standing grant; access is brokered
//!     per session (PipeWire portal / device nodes). Status is reported as
//!     `PerSession` and "Revoke now" is a no-op success with an explanatory note.
//!   - **Windows** — status is read from the privacy ConsentStore registry
//!     (`…\CapabilityAccessManager\ConsentStore\{webcam,microphone}`, NonPackaged
//!     subkey, `Value` = `Allow`/`Deny`). Screen capture has no ConsentStore
//!     entry → `Unsupported`. "Revoke now" opens the `ms-settings:` privacy
//!     deep-link rather than programmatically flipping the grant.
//!
//! The module is only compiled with the native shell (`native-shell`); the
//! headless test harness never exercises it.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, State};
// `AppHandle::config()` (used to read the bundle identifier for `tccutil`) is
// provided by the Manager trait. Only the macOS branches call it.
#[cfg(target_os = "macos")]
use tauri::Manager;

use crate::state::AppState;
use std::sync::Arc;

/// Whether the OS currently grants Pollis access to a given media device.
///
/// Mirrors the TypeScript `PermissionState` union — keep them in sync.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionState {
    /// Access is granted right now.
    Granted,
    /// Access has been explicitly denied (or is restricted by policy).
    Denied,
    /// The user has never been asked; the OS will prompt on first use.
    NotDetermined,
    /// No standing grant exists — access is brokered per session (Linux).
    PerSession,
    /// This platform exposes no queryable permission for this device.
    Unsupported,
}

/// Snapshot of the three media permissions, queried at runtime.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPermissions {
    pub camera: PermissionState,
    pub microphone: PermissionState,
    pub screen: PermissionState,
}

/// Result of a "Revoke now" request. `applied` is true only when Pollis
/// actually changed OS state (macOS `tccutil reset`); Linux/Windows return
/// `false` with a `note` explaining why.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeResult {
    pub applied: bool,
    pub note: Option<String>,
}

/// Managed state holding the "revoke on quit" preference. Mirrors the
/// `TrayState` / `tray_set_close_to_tray` pattern: the renderer pushes the
/// pref via `set_revoke_media_on_exit`, and the `ExitRequested` hook reads
/// the atomic synchronously at shutdown.
#[derive(Default)]
pub struct MediaPermissionsState {
    revoke_on_exit: AtomicBool,
}

/// The three media kinds, in the order the UI renders them.
const KINDS: [&str; 3] = ["camera", "microphone", "screen"];

// ── Commands ─────────────────────────────────────────────────────────────────

/// Report the current OS permission for camera, microphone, and screen share.
/// Queried live so it reflects changes the user makes in System Settings while
/// Pollis is running (the renderer refetches on window focus).
#[tauri::command]
pub fn get_media_permission_status() -> std::result::Result<MediaPermissions, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(MediaPermissions {
            camera: macos::camera_status(),
            microphone: macos::microphone_status(),
            screen: macos::screen_status(),
        })
    }

    #[cfg(target_os = "linux")]
    {
        // No TCC-equivalent standing grant on Linux — access is brokered
        // per session by PipeWire portals / device-node permissions.
        Ok(MediaPermissions {
            camera: PermissionState::PerSession,
            microphone: PermissionState::PerSession,
            screen: PermissionState::PerSession,
        })
    }

    #[cfg(target_os = "windows")]
    {
        Ok(MediaPermissions {
            camera: windows::consent_status("webcam"),
            microphone: windows::consent_status("microphone"),
            // Screen capture has no ConsentStore entry on Windows.
            screen: PermissionState::Unsupported,
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(MediaPermissions {
            camera: PermissionState::Unsupported,
            microphone: PermissionState::Unsupported,
            screen: PermissionState::Unsupported,
        })
    }
}

/// Deep-link to the OS privacy settings for a media `kind` ("camera" /
/// "microphone"), issue #434. An app cannot grant or revoke its own OS privacy
/// grant — only the user can, in System Settings — so this just takes them
/// there (same model as Discord/Zoom). Uses the URI schemes directly rather than
/// the shell-open bridge, whose allowlist rejects `x-apple.systempreferences:` /
/// `ms-settings:`. Linux has no per-application camera/mic model, so there is
/// nothing to deep-link to.
#[tauri::command]
pub fn open_privacy_settings(kind: String) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let anchor = match kind.as_str() {
            "camera" => "Privacy_Camera",
            "microphone" => "Privacy_Microphone",
            other => return Err(format!("unknown media kind: {other}")),
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let uri = match kind.as_str() {
            "camera" => "ms-settings:privacy-webcam",
            "microphone" => "ms-settings:privacy-microphone",
            other => return Err(format!("unknown media kind: {other}")),
        };
        // `cmd /C start "" <uri>` resolves the ms-settings: URI via the shell.
        std::process::Command::new("cmd")
            .args(["/C", "start", "", uri])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = kind;
        Err("no per-application privacy settings on this platform".into())
    }
}

/// Revoke the OS permission(s) for the given kinds ("camera"/"microphone"/
/// "screen"). Always tears down any active capture first so we never leave a
/// live camera/screen-share running against a permission we just cleared.
#[tauri::command]
pub async fn revoke_media_permissions(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    kinds: Vec<String>,
) -> std::result::Result<RevokeResult, String> {
    // Stop anything currently capturing before touching the grant. Both are
    // idempotent no-ops when nothing is active.
    let _ = pollis_core::commands::screenshare::stop_screen_share(&state).await;
    let _ = pollis_core::commands::camera::stop_camera(&state).await;

    #[cfg(target_os = "macos")]
    {
        let bundle_id = app.config().identifier.clone();
        for kind in &kinds {
            if let Some(service) = macos::tcc_service(kind) {
                // `tccutil reset` CLEARS the saved grant — macOS re-prompts on
                // next use. It does not set a permanent deny.
                let _ = std::process::Command::new("tccutil")
                    .arg("reset")
                    .arg(service)
                    .arg(&bundle_id)
                    .status();
            }
        }
        Ok(RevokeResult {
            applied: true,
            note: None,
        })
    }

    #[cfg(target_os = "linux")]
    {
        let _ = (&app, &kinds);
        Ok(RevokeResult {
            applied: false,
            note: Some(
                "Linux grants media access per session — Pollis stores no standing \
                 permission to revoke. Access ends when the session does."
                    .to_string(),
            ),
        })
    }

    #[cfg(target_os = "windows")]
    {
        use tauri_plugin_shell::ShellExt;
        let mut opened = false;
        for kind in &kinds {
            if let Some(url) = windows::ms_settings_url(kind) {
                let _ = app.shell().open(url, None);
                opened = true;
            }
        }
        let note = if opened {
            "Windows has no per-app revoke API for desktop apps — opened the \
             privacy settings so you can toggle Pollis off there."
        } else {
            "Screen-share access isn't tracked by Windows privacy settings."
        };
        Ok(RevokeResult {
            applied: false,
            note: Some(note.to_string()),
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (&app, &kinds);
        Ok(RevokeResult {
            applied: false,
            note: Some("Revoking media permissions isn't supported on this platform.".to_string()),
        })
    }
}

/// Store the "revoke system permissions when Pollis quits" preference so the
/// `ExitRequested` hook can act on it synchronously at shutdown. Mirrors
/// `tray_set_close_to_tray`.
#[tauri::command]
pub fn set_revoke_media_on_exit(state: State<'_, MediaPermissionsState>, enabled: bool) {
    state.revoke_on_exit.store(enabled, Ordering::Relaxed);
}

/// Called from the `ExitRequested` hook. If the user opted in, best-effort
/// clear the macOS TCC grants for all three kinds. Reads the atomic
/// synchronously — no async prefs fetch at exit. No-op on Linux/Windows.
pub fn revoke_on_exit_if_enabled(app: &AppHandle, state: &MediaPermissionsState) {
    if !state.revoke_on_exit.load(Ordering::Relaxed) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let bundle_id = app.config().identifier.clone();
        for kind in KINDS {
            if let Some(service) = macos::tcc_service(kind) {
                let _ = std::process::Command::new("tccutil")
                    .arg("reset")
                    .arg(service)
                    .arg(&bundle_id)
                    .status();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, state, KINDS);
    }
}

// ── macOS backend ────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use super::PermissionState;
    use cocoa::base::id;
    use objc::{class, msg_send, sel, sel_impl};

    // AVAuthorizationStatus (NSInteger). Mirrors the mapping used in
    // pollis-capture-macos/src/camera.rs.
    const AV_NOT_DETERMINED: isize = 0;
    const AV_RESTRICTED: isize = 1;
    const AV_DENIED: isize = 2;
    const AV_AUTHORIZED: isize = 3;

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        // AVMediaType constants are NSString globals.
        static AVMediaTypeVideo: id;
        static AVMediaTypeAudio: id;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        // true = the app already has Screen Recording permission.
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    fn authorization_status(media_type: id) -> PermissionState {
        let status: isize =
            unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: media_type] };
        match status {
            AV_AUTHORIZED => PermissionState::Granted,
            // Restricted (MDM/parental controls) is a denial from the user's POV.
            AV_DENIED | AV_RESTRICTED => PermissionState::Denied,
            AV_NOT_DETERMINED => PermissionState::NotDetermined,
            _ => PermissionState::NotDetermined,
        }
    }

    pub fn camera_status() -> PermissionState {
        authorization_status(unsafe { AVMediaTypeVideo })
    }

    pub fn microphone_status() -> PermissionState {
        authorization_status(unsafe { AVMediaTypeAudio })
    }

    pub fn screen_status() -> PermissionState {
        if unsafe { CGPreflightScreenCaptureAccess() } {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    /// Map a UI kind onto the `tccutil` service name.
    pub fn tcc_service(kind: &str) -> Option<&'static str> {
        match kind {
            "camera" => Some("Camera"),
            "microphone" => Some("Microphone"),
            "screen" => Some("ScreenCapture"),
            _ => None,
        }
    }
}

// ── Windows backend ──────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use super::PermissionState;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    const CONSENT_BASE: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore";

    /// Read the privacy ConsentStore verdict for a device ("webcam"/"microphone").
    /// NonPackaged (desktop) apps are governed by the NonPackaged subkey; fall
    /// back to the device root for the global default.
    pub fn consent_status(device: &str) -> PermissionState {
        let non_packaged = format!("{CONSENT_BASE}\\{device}\\NonPackaged");
        let root = format!("{CONSENT_BASE}\\{device}");
        let verdict = read_value(&non_packaged).or_else(|| read_value(&root));
        match verdict {
            Some(v) if v.eq_ignore_ascii_case("Allow") => PermissionState::Granted,
            Some(v) if v.eq_ignore_ascii_case("Deny") => PermissionState::Denied,
            _ => PermissionState::NotDetermined,
        }
    }

    /// The `ms-settings:` privacy deep-link for a UI kind.
    pub fn ms_settings_url(kind: &str) -> Option<&'static str> {
        match kind {
            "camera" => Some("ms-settings:privacy-webcam"),
            "microphone" => Some("ms-settings:privacy-microphone"),
            // No privacy page for screen capture.
            _ => None,
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_value(subkey: &str) -> Option<String> {
        let subkey_w = wide(subkey);
        let value_w = wide("Value");
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey_w.as_ptr(),
                0,
                KEY_READ,
                &mut hkey,
            ) != ERROR_SUCCESS
            {
                return None;
            }
            let mut buf = [0u16; 64];
            let mut len = (buf.len() * std::mem::size_of::<u16>()) as u32;
            let mut ty = 0u32;
            let rc = RegQueryValueExW(
                hkey,
                value_w.as_ptr(),
                std::ptr::null(),
                &mut ty,
                buf.as_mut_ptr() as *mut u8,
                &mut len,
            );
            RegCloseKey(hkey);
            if rc != ERROR_SUCCESS {
                return None;
            }
            let chars = (len as usize) / std::mem::size_of::<u16>();
            let slice: Vec<u16> = buf
                .iter()
                .take(chars)
                .copied()
                .take_while(|&c| c != 0)
                .collect();
            Some(String::from_utf16_lossy(&slice))
        }
    }
}
