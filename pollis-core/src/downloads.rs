//! Marking files that leave the app for the user's own filesystem.
//!
//! An attachment is a file a stranger sent; without a provenance marker Gatekeeper/SmartScreen never check it.
//!
//! Writing and marking are one function, [`write_downloaded_file`], so no save path can skip the marker.
//!
//! ## Per platform
//!
//! * **macOS** — `setxattr(2)` of `com.apple.quarantine` (via `libc`, no extra crate).
//! * **Windows** — the `Zone.Identifier` ADS at `<path>:Zone.Identifier`, `ZoneId=3` (internet).
//! * **Linux** — no marker exists; [`Marking::Unsupported`].

use std::path::Path;

/// What actually happened to the file's provenance marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marking {
    Marked,
    Unsupported,
    /// The platform has one and it failed. The file is still saved.
    Failed(String),
}

/// Whether this platform has a provenance marker at all.
pub const fn platform_marks_downloads() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// The `com.apple.quarantine` value.
///
/// `flags;hex-seconds;agent;uuid`; `0083` = web download, not yet approved. UUID left empty.
pub fn quarantine_value(now_unix_secs: u64) -> String {
    format!("0083;{now_unix_secs:x};Pollis;")
}

/// A Windows `Zone.Identifier` stream; CRLF because it is an INI file.
pub const ZONE_IDENTIFIER: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\n";

/// The ADS path for `path`'s `Zone.Identifier` stream.
pub fn zone_identifier_path(path: &Path) -> std::path::PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(":Zone.Identifier");
    std::path::PathBuf::from(raw)
}

/// Apply this platform's "came from the internet" marker to an existing file.
///
/// Prefer [`write_downloaded_file`]; public only for bytes already on disk.
pub fn mark_downloaded(path: &Path) -> Marking {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;

        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return Marking::Failed("path contains an interior NUL".to_string());
        };
        let name = c"com.apple.quarantine";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let value = quarantine_value(now);
        // position 0 (no resource fork), options 0 = create or replace.
        let rc = unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
                0,
            )
        };
        if rc == 0 {
            return Marking::Marked;
        }
        return Marking::Failed(format!(
            "setxattr com.apple.quarantine: {}",
            std::io::Error::last_os_error()
        ));
    }

    #[cfg(target_os = "windows")]
    {
        // Via `private_fs` so the source scan below can ban raw `std::fs` creation calls outright.
        return match crate::private_fs::write(&zone_identifier_path(path), ZONE_IDENTIFIER) {
            Ok(()) => Marking::Marked,
            // Non-NTFS destinations have no streams; the save itself succeeded.
            Err(e) => Marking::Failed(format!("write Zone.Identifier: {e}")),
        };
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        Marking::Unsupported
    }
}

/// Write `bytes` to `path` as a downloaded file, marked with this platform's
/// provenance attribute.
///
/// Through [`crate::private_fs`], so the decrypted attachment is 0600, not umask 0644.
///
/// Owner-only even at a user-picked location (whitepaper §7.2); a user can widen it.
pub fn write_downloaded_file(path: &Path, bytes: &[u8]) -> std::io::Result<Marking> {
    crate::private_fs::write(path, bytes)?;
    Ok(mark_downloaded(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quarantine_value_has_the_four_fields_launchservices_parses() {
        let value = quarantine_value(0x5f3a_1b2c);
        assert_eq!(value, "0083;5f3a1b2c;Pollis;");
        let fields: Vec<&str> = value.split(';').collect();
        assert_eq!(fields.len(), 4, "flags;timestamp;agent;uuid");
        assert_eq!(fields[0], "0083", "web download, not yet approved");
        assert_eq!(
            u64::from_str_radix(fields[1], 16).unwrap(),
            0x5f3a_1b2c,
            "the timestamp is lowercase hex seconds"
        );
        assert_eq!(fields[2], "Pollis");
    }

    #[test]
    fn the_zone_identifier_stream_is_the_internet_zone() {
        assert_eq!(
            std::str::from_utf8(ZONE_IDENTIFIER).unwrap(),
            "[ZoneTransfer]\r\nZoneId=3\r\n",
            "ZoneId=3 is URLZONE_INTERNET; CRLF because readers expect an INI file"
        );
    }

    #[test]
    fn the_zone_identifier_path_is_the_files_own_ads() {
        let path = zone_identifier_path(Path::new("/tmp/x/report.docx"));
        assert_eq!(
            path.to_string_lossy(),
            "/tmp/x/report.docx:Zone.Identifier",
            "the stream hangs off the file's own path"
        );
    }

    /// Writing always attempts the marker, and reports it on every platform.
    #[test]
    fn writing_a_download_always_reports_what_it_did_about_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("from-a-stranger.docx");
        let marking = write_downloaded_file(&path, b"payload").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"payload");
        if platform_marks_downloads() {
            assert_eq!(
                marking,
                Marking::Marked,
                "macOS and Windows both have a marker and it must have been written"
            );
        } else {
            assert_eq!(
                marking,
                Marking::Unsupported,
                "Linux has no enforced provenance marker; say so rather than claiming success"
            );
        }
    }

    /// A saved attachment must be 0600, not umask 0644 (whitepaper §7.2).
    #[cfg(unix)]
    #[test]
    fn a_saved_attachment_is_owner_only_not_umask() {
        if !crate::private_fs::owner_only_is_enforced_by_mode() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payslip.pdf");
        write_downloaded_file(&path, b"decrypted attachment bytes").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            crate::private_fs::FILE_MODE,
            "a saved attachment must be 0600, not whatever the umask says"
        );
    }

    /// A pre-existing world-readable file is tightened too.
    #[cfg(unix)]
    #[test]
    fn overwriting_an_existing_loose_file_tightens_it() {
        if !crate::private_fs::owner_only_is_enforced_by_mode() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("again.pdf");
        std::fs::write(&path, b"older save").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_downloaded_file(&path, b"newer save").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            crate::private_fs::FILE_MODE
        );
    }

    /// The guard the export path already had and the download path did not.
    /// Scanning source keeps the next save site honest.
    #[test]
    fn nothing_on_the_download_path_creates_a_file_outside_private_fs() {
        for (name, src) in [
            ("downloads.rs", include_str!("downloads.rs")),
            ("commands/r2.rs", include_str!("commands/r2.rs")),
        ] {
            let body = src.split("#[cfg(test)]").next().unwrap();
            for banned in [
                "std::fs::write",
                "std::fs::File::create",
                "std::fs::create_dir_all",
                "tokio::fs::write",
                "tokio::fs::create_dir_all",
                ".create(true)",
            ] {
                assert!(
                    !body.contains(banned),
                    "{name} must create files through private_fs; found `{banned}`"
                );
            }
        }
    }

    /// macOS only: read the attribute back off the file.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_quarantine_is_readable_back_off_the_file() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.dmg");
        assert_eq!(write_downloaded_file(&path, b"x").unwrap(), Marking::Marked);

        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = c"com.apple.quarantine";
        let mut buf = [0u8; 256];
        let len = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
                0,
            )
        };
        assert!(len > 0, "the attribute must be present after the write");
        let value = std::str::from_utf8(&buf[..len as usize]).unwrap();
        assert!(value.starts_with("0083;"), "got {value}");
        assert!(value.contains(";Pollis;"), "got {value}");
    }

    /// Windows only: the alternate data stream is there and says internet zone.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_mark_of_the_web_is_readable_back_off_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macros.docm");
        assert_eq!(write_downloaded_file(&path, b"x").unwrap(), Marking::Marked);

        let stream = std::fs::read(zone_identifier_path(&path)).expect("read the ADS back");
        assert_eq!(stream, ZONE_IDENTIFIER);
    }
}
