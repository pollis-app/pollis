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
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
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
    /// Translocated because an older release's updater stamped the installed
    /// bundle itself (see [`heal_installed_copies`]); the stamp is gone and a
    /// plain relaunch from Applications is enough.
    TranslocatedHealed,
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
    let location = classify_exe_path(&exe.to_string_lossy())?;
    if location == ReadOnlyLocation::Translocated
        && heal_installed_copies(&exe).iter().any(|(_, heal)| heal.relaunch_is_clean())
    {
        return Some(ReadOnlyLocation::TranslocatedHealed);
    }
    Some(location)
}

/// Releases 1.12–1.17 shipped `LSFileQuarantineEnabled`, which made macOS stamp
/// `com.apple.quarantine` on every file the process wrote — including the
/// bundle the in-app updater installed. Finder never moved that bundle, so the
/// next launch translocated it, and a translocated app cannot update: every
/// update broke the one after it. Those bundles carry Pollis's own name in the
/// stamp's agent field; only that stamp is removed, and only from the standard
/// install locations. A browser's stamp on a fresh download is left for
/// Gatekeeper exactly as before.
pub const QUARANTINE_AGENT: &str = "Pollis";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heal {
    /// Pollis's own stamp was removed; the next Finder launch is a normal one.
    Healed,
    /// No stamp at all.
    AlreadyClean,
    /// Stamped by something else (a browser, an archive utility): not ours to touch.
    ForeignStamp,
    /// Nothing installed at that location.
    Missing,
    Failed(String),
}

impl Heal {
    fn relaunch_is_clean(&self) -> bool {
        matches!(self, Heal::Healed | Heal::AlreadyClean)
    }
}

fn quarantine_agent(value: &str) -> Option<&str> {
    value.split(';').nth(2)
}

/// The `.app` an executable at `<bundle>/Contents/MacOS/<exe>` belongs to.
fn bundle_of(exe: &Path) -> Option<&Path> {
    let bundle = exe.ancestors().nth(3)?;
    (bundle.extension() == Some(OsStr::new("app"))).then_some(bundle)
}

/// Where a Finder install of this bundle would live, plus the running bundle
/// itself when it is a real install rather than a translocated or mounted copy.
fn candidate_installs(exe: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let Some(bundle) = bundle_of(exe) else {
        return Vec::new();
    };
    let Some(name) = bundle.file_name() else {
        return Vec::new();
    };
    let mut out = vec![PathBuf::from("/Applications").join(name)];
    if let Some(home) = home {
        out.push(home.join("Applications").join(name));
    }
    if classify_exe_path(&exe.to_string_lossy()).is_none() && !out.iter().any(|p| p == bundle) {
        out.push(bundle.to_path_buf());
    }
    out
}

pub fn heal_installed_copies(exe: &Path) -> Vec<(PathBuf, Heal)> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    candidate_installs(exe, home.as_deref())
        .into_iter()
        .map(|path| {
            let heal = heal_location(&path);
            (path, heal)
        })
        .collect()
}

/// Run at startup so a bundle stamped by an older release is clean before the
/// next update is even offered.
pub fn heal_self_quarantine_at_startup() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    for (path, heal) in heal_installed_copies(&exe) {
        match heal {
            Heal::Healed => pollis_core::diag!(
                "[startup] removed Pollis's own quarantine stamp from {}",
                path.display()
            ),
            Heal::Failed(e) => pollis_core::diag!(
                "[startup] could not remove the quarantine stamp from {}: {e}",
                path.display()
            ),
            _ => {}
        }
    }
}

fn heal_location(path: &Path) -> Heal {
    if path.symlink_metadata().is_err() {
        return Heal::Missing;
    }
    match read_quarantine(path) {
        None => Heal::AlreadyClean,
        Some(value) if quarantine_agent(&value) == Some(QUARANTINE_AGENT) => {
            match remove_quarantine_tree(path) {
                Ok(()) => Heal::Healed,
                Err(e) => Heal::Failed(e.to_string()),
            }
        }
        Some(_) => Heal::ForeignStamp,
    }
}

#[cfg(not(target_os = "macos"))]
fn read_quarantine(_path: &Path) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn remove_quarantine_tree(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
const QUARANTINE_XATTR: &std::ffi::CStr = c"com.apple.quarantine";

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("path contains an interior NUL"))
}

#[cfg(target_os = "macos")]
fn read_quarantine(path: &Path) -> Option<String> {
    let c_path = c_path(path).ok()?;
    let mut buf = [0u8; 1024];
    let len = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            QUARANTINE_XATTR.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if len < 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

#[cfg(target_os = "macos")]
fn remove_quarantine(path: &Path) -> std::io::Result<()> {
    let c_path = c_path(path)?;
    let rc = unsafe { libc::removexattr(c_path.as_ptr(), QUARANTINE_XATTR.as_ptr(), libc::XATTR_NOFOLLOW) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOATTR) {
        return Ok(());
    }
    Err(err)
}

/// Every entry of the bundle, not just its root: the stamp was applied per file
/// as each was written, so a single-level removal would leave the bundle's
/// contents still marked. Symlinks are handled by name, never followed.
#[cfg(target_os = "macos")]
fn remove_quarantine_tree(path: &Path) -> std::io::Result<()> {
    remove_quarantine(path)?;
    if path.symlink_metadata()?.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove_quarantine_tree(&entry?.path())?;
        }
    }
    Ok(())
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

    #[test]
    fn the_agent_is_the_third_field_of_the_stamp() {
        assert_eq!(quarantine_agent("0081;6ad0d826;Pollis;"), Some("Pollis"));
        assert_eq!(quarantine_agent("0083;6ad0d826;Safari;ABCD-1234"), Some("Safari"));
        assert_eq!(quarantine_agent("0081"), None);
    }

    #[test]
    fn the_bundle_is_three_levels_above_the_executable() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Pollis.app/Contents/MacOS/pollis")),
            Some(Path::new("/Applications/Pollis.app"))
        );
        assert_eq!(bundle_of(Path::new("/usr/local/bin/pollis")), None);
    }

    #[test]
    fn a_translocated_app_looks_for_its_finder_installs_only() {
        let exe = Path::new("/private/var/folders/xy/T/AppTranslocation/1F2E/d/Pollis.app/Contents/MacOS/pollis");
        assert_eq!(
            candidate_installs(exe, Some(Path::new("/Users/me"))),
            vec![
                PathBuf::from("/Applications/Pollis.app"),
                PathBuf::from("/Users/me/Applications/Pollis.app"),
            ]
        );
    }

    #[test]
    fn an_installed_app_includes_its_own_bundle_once() {
        let exe = Path::new("/Applications/Pollis.app/Contents/MacOS/pollis");
        assert_eq!(
            candidate_installs(exe, None),
            vec![PathBuf::from("/Applications/Pollis.app")]
        );
        let exe = Path::new("/opt/Pollis.app/Contents/MacOS/pollis");
        assert_eq!(
            candidate_installs(exe, None),
            vec![PathBuf::from("/Applications/Pollis.app"), PathBuf::from("/opt/Pollis.app")]
        );
    }

    #[cfg(target_os = "macos")]
    fn stamped_bundle(stamp: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pollis-heal-{}-{}", std::process::id(), stamp.len()));
        let bundle = root.join("Pollis.app");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/pollis"), b"x").unwrap();
        for p in [&bundle, &bundle.join("Contents"), &bundle.join("Contents/MacOS/pollis")] {
            let c = c_path(p).unwrap();
            let rc = unsafe {
                libc::setxattr(
                    c.as_ptr(),
                    QUARANTINE_XATTR.as_ptr(),
                    stamp.as_ptr() as *const libc::c_void,
                    stamp.len(),
                    0,
                    0,
                )
            };
            assert_eq!(rc, 0);
        }
        bundle
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn our_own_stamp_is_removed_from_the_whole_bundle() {
        let bundle = stamped_bundle("0081;6ad0d826;Pollis;");
        assert_eq!(heal_location(&bundle), Heal::Healed);
        assert_eq!(read_quarantine(&bundle), None);
        assert_eq!(read_quarantine(&bundle.join("Contents/MacOS/pollis")), None);
        assert_eq!(heal_location(&bundle), Heal::AlreadyClean);
        std::fs::remove_dir_all(bundle.parent().unwrap()).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_browser_stamp_is_left_for_gatekeeper() {
        let bundle = stamped_bundle("0083;6ad0d826;Safari;0000-1111");
        assert_eq!(heal_location(&bundle), Heal::ForeignStamp);
        assert_eq!(read_quarantine(&bundle).as_deref(), Some("0083;6ad0d826;Safari;0000-1111"));
        std::fs::remove_dir_all(bundle.parent().unwrap()).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nothing_installed_is_reported_not_invented() {
        assert_eq!(heal_location(Path::new("/Applications/Definitely-Not-Here.app")), Heal::Missing);
    }
}
