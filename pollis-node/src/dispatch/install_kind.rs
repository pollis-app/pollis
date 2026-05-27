// Electron-side replacement for the Tauri `install_kind` command.
//
// Detect when Pollis is running from a system package manager that owns the
// install (AUR `pollis` PKGBUILD, raw .deb/.rpm). electron-updater can only
// auto-update AppImage on Linux — trying to swap a system-installed binary
// either silently fails or (the case that bit us) flips `update_required`,
// runs `app.relaunch()` against a binary that never got replaced, and the
// user is signed out for nothing.
//
// Detection signals we have without Tauri's BundleType:
//   - process.env.APPIMAGE: set by AppImage runtime; if present we ARE in an
//     AppImage and the auto-updater path is valid.
//   - argv[0] / /proc/self/exe: a binary under /usr, /opt, or /bin lives in
//     a system path that only a package manager writes to.
//   - /etc/os-release: distinguishes AUR-on-Arch (ID=arch or ID_LIKE=arch)
//     from a regular Debian/Fedora install, which tunes the displayed
//     update command.
//
// macOS + Windows: no managed cases today (Mac App Store + Microsoft Store
// aren't shipping yet). Return None so the in-app updater runs.

use napi::bindgen_prelude::*;
use serde_json::json;

pub async fn dispatch(
    cmd: &str,
    _args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "detect_managed_install" => Some(Ok(detect()
            .map(|m| {
                json!({
                    "kind": m.kind,
                    "display_name": m.display_name,
                    "update_command": m.update_command,
                })
            })
            .unwrap_or(serde_json::Value::Null))),
        _ => None,
    }
}

struct Managed {
    kind: &'static str,
    display_name: &'static str,
    update_command: Option<&'static str>,
}

fn detect() -> Option<Managed> {
    #[cfg(target_os = "linux")]
    {
        return detect_linux();
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn detect_linux() -> Option<Managed> {
    // AppImage sets APPIMAGE to the absolute path of the .AppImage file
    // before launching the embedded binary. If it's there, we're not
    // package-managed and electron-updater's AppImage path is fine.
    if std::env::var_os("APPIMAGE").is_some() {
        return None;
    }

    // Binary must live in a system path to be package-managed. AppImage
    // mounts under /tmp/.mount_*; an unpacked tarball would typically live
    // in $HOME. Anything else (/usr, /opt, /bin, /usr/local) is owned by
    // a package manager or sysadmin — either way the in-app updater can't
    // replace it.
    let Ok(exe) = std::fs::read_link("/proc/self/exe") else {
        return None;
    };
    let exe = exe.to_string_lossy().into_owned();
    let system_path = exe.starts_with("/usr/")
        || exe.starts_with("/opt/")
        || exe.starts_with("/bin/")
        || exe.starts_with("/sbin/");
    if !system_path {
        return None;
    }

    if os_release_is_arch() {
        // AUR PKGBUILD repackages our .deb under /opt/Pollis/, which is
        // the path Electron's main binary ends up at on Arch installs.
        return Some(Managed {
            kind: "aur",
            display_name: "the AUR (Arch User Repository)",
            // -S, not -Syu: single-package update, don't surprise the user
            // with a full-system upgrade.
            update_command: Some("yay -S pollis"),
        });
    }

    // Some other distro shipped Pollis from a system package. We don't
    // know the package manager; surface a generic message rather than
    // guessing apt vs. dnf vs. zypper.
    Some(Managed {
        kind: "linux_system",
        display_name: "your distribution's package manager",
        update_command: None,
    })
}

#[cfg(target_os = "linux")]
fn os_release_is_arch() -> bool {
    let Ok(contents) = std::fs::read_to_string("/etc/os-release") else {
        return false;
    };
    contents.lines().any(|line| {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ID=") {
            return strip_quotes(rest) == "arch";
        }
        if let Some(rest) = line.strip_prefix("ID_LIKE=") {
            return strip_quotes(rest)
                .split_whitespace()
                .any(|tok| tok == "arch");
        }
        false
    })
}

#[cfg(target_os = "linux")]
fn strip_quotes(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '"' || c == '\'')
}
