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

    pub fn update_command(self) -> &'static str {
        match self {
            ManagedInstallKind::Aur => "yay -Syu pollis",
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
