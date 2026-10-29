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

/// Create the data directory (owner-only) and mark it excluded from backup.
/// Run once at init, before anything writes under `dirs_path()`; idempotent.
pub fn prepare(dir: &Path) -> std::io::Result<()> {
    crate::private_fs::create_dir_all(dir)?;
    exclude_from_backup(dir)
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
