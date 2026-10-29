//! OS camera / mic / screen permissions. Not a core shim: TCC, ConsentStore and `ms-settings:` are shell concerns.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, State};
#[cfg(target_os = "macos")]
use tauri::Manager;

use crate::state::AppState;
use std::sync::Arc;

/// Mirrors the TypeScript `PermissionState` union.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionState {
    Granted,
    Denied,
    NotDetermined,
    /// Brokered per session (Linux).
    PerSession,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPermissions {
    pub camera: PermissionState,
    pub microphone: PermissionState,
    pub screen: PermissionState,
}

/// `applied` is true only when Pollis changed OS state (macOS `tccutil reset`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeResult {
    pub applied: bool,
    pub note: Option<String>,
}

/// "Revoke on quit" pref, read synchronously by the `ExitRequested` hook.
#[derive(Default)]
pub struct MediaPermissionsState {
    revoke_on_exit: AtomicBool,
}

const KINDS: [&str; 3] = ["camera", "microphone", "screen"];

/// Queried live so changes made in System Settings show up on refocus.
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

/// Deep-link to OS privacy settings (#434); direct URIs because the shell-open allowlist rejects these schemes.
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

/// Revoke OS grants, tearing down any live capture first.
#[tauri::command]
pub async fn revoke_media_permissions(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    kinds: Vec<String>,
) -> std::result::Result<RevokeResult, String> {
    let _ = pollis_core::commands::screenshare::stop_screen_share(&state).await;
    let _ = pollis_core::commands::camera::stop_camera(&state).await;

    #[cfg(target_os = "macos")]
    {
        let bundle_id = app.config().identifier.clone();
        for kind in &kinds {
            if let Some(service) = macos::tcc_service(kind) {
                // Clears the grant (re-prompts on next use); not a permanent deny.
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

#[tauri::command]
pub fn set_revoke_media_on_exit(state: State<'_, MediaPermissionsState>, enabled: bool) {
    state.revoke_on_exit.store(enabled, Ordering::Relaxed);
}

/// If opted in, best-effort clear macOS TCC grants at exit; no-op elsewhere.
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

#[cfg(target_os = "macos")]
mod macos {
    use super::PermissionState;
    use cocoa::base::id;
    use objc::{class, msg_send, sel, sel_impl};

    const AV_NOT_DETERMINED: isize = 0;
    const AV_RESTRICTED: isize = 1;
    const AV_DENIED: isize = 2;
    const AV_AUTHORIZED: isize = 3;

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeVideo: id;
        static AVMediaTypeAudio: id;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    fn authorization_status(media_type: id) -> PermissionState {
        let status: isize =
            unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: media_type] };
        match status {
            AV_AUTHORIZED => PermissionState::Granted,
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

    pub fn tcc_service(kind: &str) -> Option<&'static str> {
        match kind {
            "camera" => Some("Camera"),
            "microphone" => Some("Microphone"),
            "screen" => Some("ScreenCapture"),
            _ => None,
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::PermissionState;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    const CONSENT_BASE: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore";

    /// NonPackaged subkey first, falling back to the device root.
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

    pub fn ms_settings_url(kind: &str) -> Option<&'static str> {
        match kind {
            "camera" => Some("ms-settings:privacy-webcam"),
            "microphone" => Some("ms-settings:privacy-microphone"),
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
