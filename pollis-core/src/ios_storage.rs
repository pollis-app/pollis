//! iOS-only placement of the core's data directory (#1306).
//!
//! The directory is `Library/Application Support/pollis` inside the app
//! container: persistent, never purged, and not user-visible. It is excluded
//! from iCloud/iTunes backup so a restore onto another phone cannot carry the
//! state without the Keychain item (which is `ThisDeviceOnly` and does not
//! travel) — the same "a new device starts empty" rule as everywhere else.

#![cfg(target_os = "ios")]

use std::path::{Path, PathBuf};

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::url::CFURL;

/// `$HOME/Library/Application Support/pollis`. On iOS `$HOME` is the app's
/// sandbox container, so this never leaves it.
pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/pollis")
}

/// Create the data directory (owner-only), mark it excluded from backup, and make sure its
/// files stay readable after the first unlock (the Notification Service Extension runs on a
/// locked phone, #1599). Run once at init, before anything writes under `dirs_path()`; idempotent.
pub fn prepare(dir: &Path) -> std::io::Result<()> {
    crate::private_fs::create_dir_all(dir)?;
    exclude_from_backup(dir)?;
    readable_after_first_unlock(dir);
    Ok(())
}

/// The app's side of the #1599 relocation: on the first launch with an override, move the old
/// `Library/Application Support/pollis` into it, then [`prepare`] it.
///
/// Runs only when the override directory holds nothing yet and the old one exists. Same volume
/// (the usual case: the app and App Group containers share the data partition) is one atomic
/// `rename`. Otherwise the tree is copied into a sibling staging directory, fsynced, and renamed
/// into place, and only then is the old tree removed, so an interruption leaves the old
/// directory whole and the next launch starts over.
pub fn migrate_into_override(new_dir: &Path) -> std::io::Result<()> {
    let old_dir = data_dir();
    crate::notify::host::migrate_dir(&old_dir, new_dir)?;
    prepare(new_dir)
}

/// `F_GETPROTECTIONCLASS` / `F_SETPROTECTIONCLASS` from `<sys/fcntl.h>` (not in the libc crate).
const F_GETPROTECTIONCLASS: libc::c_int = 63;
const F_SETPROTECTIONCLASS: libc::c_int = 64;
/// Data Protection classes A (Complete) and B (CompleteUnlessOpen) lock a file while the phone
/// is locked; C is CompleteUntilFirstUserAuthentication.
const PROTECTION_CLASS_A: libc::c_int = 1;
const PROTECTION_CLASS_B: libc::c_int = 2;
const PROTECTION_CLASS_C: libc::c_int = 3;

/// iOS gives app files class C by default, which an extension can read on a locked phone. Only
/// a stricter class (A/B, from a future entitlement or a copy) is relaxed to C, on the directory
/// (which new files inherit) and the files directly in it; anything else is left as it is.
/// Best-effort: a failure costs rich notifications, never startup.
fn readable_after_first_unlock(dir: &Path) {
    let mut paths = vec![dir.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        paths.extend(
            entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|e| e.path()),
        );
    }
    for path in paths {
        if let Err(e) = relax_protection(&path) {
            crate::diag!("[storage] protection class of {} not checked: {e}", path.display());
        }
    }
}

fn relax_protection(path: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(path)?;
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is open for the duration of both calls.
    let class = unsafe { libc::fcntl(fd, F_GETPROTECTIONCLASS) };
    if class < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if class == PROTECTION_CLASS_A || class == PROTECTION_CLASS_B {
        // SAFETY: as above.
        if unsafe { libc::fcntl(fd, F_SETPROTECTIONCLASS, PROTECTION_CLASS_C) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Sets `NSURLIsExcludedFromBackupKey` on `dir`. The flag lives on the
/// directory and covers everything beneath it, including files written later.
fn exclude_from_backup(dir: &Path) -> std::io::Result<()> {
    let url = CFURL::from_path(dir, true).ok_or_else(|| {
        std::io::Error::other(format!("not a valid file URL: {}", dir.display()))
    })?;
    let value = CFBoolean::true_value();
    let mut error = std::ptr::null_mut();
    // SAFETY: `url` and `value` are live CF objects for the duration of the
    // call, the key is a CoreFoundation constant, and `error` is an out-param
    // that CF fills with an owned CFError only on failure.
    let ok = unsafe {
        core_foundation_sys::url::CFURLSetResourcePropertyForKey(
            url.as_concrete_TypeRef(),
            core_foundation_sys::url::kCFURLIsExcludedFromBackupKey,
            value.as_CFTypeRef(),
            &mut error,
        )
    };
    if ok == 0 {
        if !error.is_null() {
            // SAFETY: on failure CF hands us ownership of `error` (create rule).
            let err = unsafe { core_foundation::error::CFError::wrap_under_create_rule(error) };
            return Err(std::io::Error::other(format!(
                "exclude {} from backup: {}",
                dir.display(),
                err.description()
            )));
        }
        return Err(std::io::Error::other(format!(
            "exclude {} from backup failed",
            dir.display()
        )));
    }
    Ok(())
}
