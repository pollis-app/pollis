//! Marking files that leave the app for the user's own filesystem.
//!
//! An attachment is a file a stranger sent. When "save as…" wrote it with a
//! plain `writeFile`, it landed on the disk indistinguishable from something
//! the user made themselves — no quarantine attribute on macOS, no
//! mark-of-the-web on Windows. Both operating systems have a whole layer of
//! defence keyed on exactly that marker (Gatekeeper's "downloaded from the
//! internet" prompt and its notarization check; SmartScreen, Office's Protected
//! View, and the script-host warnings), and every one of them was silently
//! skipped for anything saved out of Pollis. A `.dmg`, a `.docx` with macros,
//! an `.hta` — all of them opened as trusted local files.
//!
//! The marker is not something the renderer can be trusted to add afterwards,
//! and it is not something to remember at each save site. So the write and the
//! marking are the same function: [`write_downloaded_file`]. There is no way to
//! use this module to write a downloaded file WITHOUT marking it, which is the
//! only property that survives the next person adding a save path.
//!
//! ## Per platform
//!
//! * **macOS** — `setxattr(2)` of `com.apple.quarantine`. The value is the
//!   four-field `flags;hex-timestamp;agent;uuid` string LaunchServices parses;
//!   the flag bits say "web download, not yet approved by the user", which is
//!   what makes Gatekeeper evaluate the file on first open. `libc` rather than
//!   the `xattr` crate: it is one call, `libc` is already a dependency on every
//!   unix target here, and this avoids adding a crate to the supply chain of a
//!   security-critical binary for a single FFI signature.
//! * **Windows** — the `Zone.Identifier` alternate data stream, written as a
//!   plain file at `<path>:Zone.Identifier`. `ZoneId=3` is URLZONE_INTERNET.
//!   This is the same thing browsers write, and needs no API: NTFS exposes an
//!   ADS through the ordinary file path syntax.
//! * **Linux** — nothing to do. There is no OS-level provenance marker with any
//!   enforcement behind it, so [`Marking::Unsupported`] is the honest answer
//!   rather than a silent success.

use std::path::Path;

/// What actually happened to the file's provenance marker.
///
/// Returned rather than swallowed so a caller (and a test) can tell "the
/// platform has no marker" apart from "the marker failed", which are very
/// different facts and used to be indistinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marking {
    /// The marker was written.
    Marked,
    /// This platform has no provenance marker.
    Unsupported,
    /// The platform has one and it could not be written. The file is still on
    /// disk — refusing to save someone's attachment because an xattr failed
    /// would be the wrong trade — but the caller is told.
    Failed(String),
}

/// Whether this platform has a provenance marker at all. A value, so tests can
/// branch on it instead of restating the `cfg` soup.
pub const fn platform_marks_downloads() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// The `com.apple.quarantine` value.
///
/// `flags;hex-seconds;agent;uuid`. `0083` = the web-download type plus the
/// "user has not approved this yet" bit, i.e. what a browser writes. The UUID
/// field is left empty: it keys an optional LaunchServices database entry that
/// only matters for the "where did this come from" detail in the prompt, and
/// inventing one would be inventing provenance we do not have.
///
/// Split out as a pure function so its shape is tested on every platform, not
/// only on the one that can apply it.
pub fn quarantine_value(now_unix_secs: u64) -> String {
    format!("0083;{now_unix_secs:x};Pollis;")
}

/// The bytes of a Windows `Zone.Identifier` stream. CRLF, because the stream is
/// an INI file and that is what every reader of it expects.
pub const ZONE_IDENTIFIER: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\n";

/// The ADS path for `path`'s `Zone.Identifier` stream.
///
/// Pure and platform-independent so the naming is tested everywhere; only the
/// Windows arm of [`mark_downloaded`] ever writes to it.
pub fn zone_identifier_path(path: &Path) -> std::path::PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(":Zone.Identifier");
    std::path::PathBuf::from(raw)
}

/// Apply this platform's "came from the internet" marker to an existing file.
///
/// Prefer [`write_downloaded_file`], which cannot be called without marking.
/// This is public only for the case where the bytes are already on disk.
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
        // setxattr(path, name, value, size, position, options); position is 0
        // for anything but a resource fork, options 0 = create or replace.
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
        return match std::fs::write(zone_identifier_path(path), ZONE_IDENTIFIER) {
            Ok(()) => Marking::Marked,
            // A non-NTFS destination (a FAT32 stick, a network share) has no
            // streams. The save itself succeeded; say so honestly.
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
/// The two steps are one call on purpose — see the module docs.
pub fn write_downloaded_file(path: &Path, bytes: &[u8]) -> std::io::Result<Marking> {
    std::fs::write(path, bytes)?;
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

    /// The point of the module: writing goes through one function, and that
    /// function always attempts the marker. Asserted on whatever platform the
    /// suite runs on — `Unsupported` on Linux (where the CI for this repo
    /// runs), `Marked` on macOS and Windows, and never a silent success that
    /// hides a failed attempt.
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

    /// macOS only: read the attribute back off the file. Does not run on Linux
    /// (there is nothing to read), which is stated rather than skipped
    /// silently.
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
