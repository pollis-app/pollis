//! Detect when Pollis is running from a system package manager that owns
//! the install (e.g. AUR / pacman). Tauri's auto-updater can't replace
//! a package-managed binary — on Arch the AUR `PKGBUILD` extracts our `.deb`,
//! so the binary identifies as `bundle_type=Deb`, the updater dispatches to
//! `install_deb`, and `dpkg -i` either fails (no dpkg on Arch) or returns
//! `InvalidUpdaterFormat` if the manifest URL points to a non-`.deb` file.
//!
//! When this returns `Some`, the frontend replaces the auto-updater flow
//! with a hard-stop screen telling the user to update via their package
//! manager. This is also the gate we'll extend for Mac App Store / Microsoft
//! Store builds, which forbid in-app auto-updates.
//!
//! Detection is deliberately conservative: we only claim a managed install
//! when we have strong evidence. False negatives fall back to the regular
//! auto-updater (which then either succeeds or shows its own error).

use serde::Serialize;
use tauri::utils::{config::BundleType, platform::bundle_type};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedInstallKind {
    /// Arch User Repository — install came from `yay`/`paru`/`pacman` via
    /// the `pollis` AUR PKGBUILD that repackages our `.deb` artifact.
    Aur,
}

impl ManagedInstallKind {
    pub fn display_name(self) -> &'static str {
        match self {
            ManagedInstallKind::Aur => "the AUR (Arch User Repository)",
        }
    }

    /// Single-package update command. Deliberately *not* `-Syu` /
    /// `-Syyu` — those are full-system upgrades and would update the
    /// user's kernel and every other package. The user came here to
    /// update Pollis, nothing else.
    pub fn update_command(self) -> &'static str {
        match self {
            ManagedInstallKind::Aur => "yay -S pollis",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ManagedInstallInfo {
    pub kind: ManagedInstallKind,
    pub display_name: &'static str,
    pub update_command: &'static str,
}

impl From<ManagedInstallKind> for ManagedInstallInfo {
    fn from(kind: ManagedInstallKind) -> Self {
        Self {
            kind,
            display_name: kind.display_name(),
            update_command: kind.update_command(),
        }
    }
}

/// Inspect the running binary + host to decide whether a system package
/// manager owns this install. Returns `None` on user-installed builds
/// (AppImage, .dmg, direct .exe) where the in-app updater is the right
/// path.
pub fn detect() -> Option<ManagedInstallKind> {
    #[cfg(target_os = "linux")]
    {
        // AUR PKGBUILD installs from our .deb, so the bundled binary's
        // sentinel still says "Deb". Pair that with /etc/os-release to
        // distinguish AUR-on-Arch from a regular Debian/Ubuntu .deb install.
        let is_deb_bundle = matches!(bundle_type(), Some(BundleType::Deb));
        if is_deb_bundle && os_release_is_arch() {
            return Some(ManagedInstallKind::Aur);
        }
    }
    let _ = bundle_type;
    None
}

#[cfg(target_os = "linux")]
fn os_release_is_arch() -> bool {
    let Ok(contents) = std::fs::read_to_string("/etc/os-release") else {
        return false;
    };
    contents.lines().any(|line| {
        let line = line.trim();
        // ID=arch (canonical Arch) or ID_LIKE=arch (Manjaro, EndeavourOS, etc.)
        // Values may be quoted: ID="arch".
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

#[tauri::command]
pub fn detect_managed_install() -> Option<ManagedInstallInfo> {
    detect().map(Into::into)
}

/// Where the running app lives when the in-app updater cannot replace it.
///
/// Tauri's macOS updater swaps the `.app` bundle in place, so it needs the
/// bundle's directory to be writable. Two launch locations are read-only and
/// fail with "Read-only file system (os error 30)" partway through an update:
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyLocation {
    /// Launched straight from the mounted disk image (`/Volumes/...`) instead
    /// of from a copy in Applications.
    DiskImage,
    /// macOS App Translocation: a quarantined app run from a randomized,
    /// read-only mount (`.../AppTranslocation/...`) until it is moved in Finder.
    Translocated,
}

/// Classify an executable path. Pure, so the rule is testable on every OS.
fn classify_exe_path(path: &str) -> Option<ReadOnlyLocation> {
    if path.contains("/AppTranslocation/") {
        return Some(ReadOnlyLocation::Translocated);
    }
    if path.starts_with("/Volumes/") {
        return Some(ReadOnlyLocation::DiskImage);
    }
    None
}

/// `Some` when the running app is in a location the updater cannot write to,
/// so the update screen can say how to fix it before attempting an install
/// that is guaranteed to fail. `None` everywhere but macOS.
#[tauri::command]
pub fn detect_read_only_location() -> Option<ReadOnlyLocation> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    classify_exe_path(&exe.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_applications_install_is_writable() {
        assert_eq!(classify_exe_path("/Applications/Pollis.app/Contents/MacOS/pollis"), None);
        assert_eq!(classify_exe_path("/Users/me/Applications/Pollis.app/Contents/MacOS/pollis"), None);
    }

    #[test]
    fn running_from_the_mounted_dmg_is_read_only() {
        assert_eq!(
            classify_exe_path("/Volumes/Pollis 1.14.0/Pollis.app/Contents/MacOS/pollis"),
            Some(ReadOnlyLocation::DiskImage)
        );
    }

    #[test]
    fn a_translocated_app_is_read_only() {
        assert_eq!(
            classify_exe_path(
                "/private/var/folders/xy/abc/T/AppTranslocation/1F2E-33/d/Pollis.app/Contents/MacOS/pollis"
            ),
            Some(ReadOnlyLocation::Translocated)
        );
    }

    #[test]
    fn translocation_wins_even_if_the_original_was_on_a_volume() {
        assert_eq!(
            classify_exe_path("/Volumes/x/AppTranslocation/1/d/Pollis.app/Contents/MacOS/pollis"),
            Some(ReadOnlyLocation::Translocated)
        );
    }
}
