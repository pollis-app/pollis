//! The switch that has to be thrown before the in-app terminal exists.
//!
//! `terminal_open` spawns the user's login shell. That is the most powerful
//! thing this process can be asked to do, and until now the renderer could ask
//! for it with one `invoke("terminal_open")` — no gate, no consent, no trace.
//! Anything that reaches script execution inside the webview (a renderer bug, a
//! dependency, a content-injection path) therefore reached `$SHELL -l` with the
//! user's full environment and a bidirectional byte pipe. The terminal pane is a
//! power-user convenience; arbitrary code execution is not a convenience the
//! default install should ship switched on.
//!
//! So the shell is **off unless the user turned it on**, and the answer lives
//! here rather than in the renderer for the same reason the auto-lock deadline
//! does: a preference the webview holds is a preference the webview can lie
//! about. This module owns the on-disk flag and every terminal entry point
//! consults it, so "enabled" is a fact about the device, not a claim in an IPC
//! argument.
//!
//! **Default OFF, and the default is the missing file.** A fresh install has no
//! `device-settings.json`; an unreadable file, a truncated write and a file an
//! older build wrote without the key all read back the same way. There is no
//! state in which corrupt or absent means "on" — the only thing that enables the
//! shell is an intact file this module wrote saying so.
//!
//! Device-local by design: it describes one machine's posture, so it is not
//! synced and not part of the account. Same reasoning as the auto-lock timeout.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::local::dirs_path;
use crate::error::{Error, Result};

/// Device-local settings that must be decided in Rust rather than in the
/// renderer. One file, so the next such flag does not mint another.
///
/// Every field needs `#[serde(default)]` and a `Default` that is the SAFE
/// answer: a file an older build wrote has no key for a newer flag, and the
/// value it then takes has to be the one that grants nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSettings {
    /// Whether the in-app terminal pane may spawn a shell on this device.
    #[serde(default)]
    pub terminal_enabled: bool,
}

/// Where the flag lives. Beside `accounts.json` in the app data directory.
fn settings_path() -> PathBuf {
    dirs_path().join("device-settings.json")
}

/// Read settings out of `path`, failing safe.
///
/// A missing file, an unreadable file and an unparseable file are all the same
/// answer — [`DeviceSettings::default`], i.e. every capability off. Nothing here
/// returns an error, because there is no caller that could do anything useful
/// with one: the question is "is this capability enabled", and anything short of
/// a file that clearly says yes means no.
pub fn read_at(path: &Path) -> DeviceSettings {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return DeviceSettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Persist `settings` to `path`, owner-only, creating the parent directory.
pub fn write_at(path: &Path, settings: &DeviceSettings) -> Result<()> {
    if let Some(parent) = path.parent() {
        crate::private_fs::create_dir_all(parent)
            .map_err(|e| Error::Other(anyhow::anyhow!("create data dir: {e}")))?;
    }
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize device settings: {e}")))?;
    crate::private_fs::write(path, &json)
        .map_err(|e| Error::Other(anyhow::anyhow!("write device-settings.json: {e}")))?;
    Ok(())
}

/// The device's settings as they stand right now.
pub fn read_device_settings() -> DeviceSettings {
    read_at(&settings_path())
}

/// The gate itself, as a pure function of the settings, so the decision can be
/// tested without a filesystem and cannot drift from what the entry points ask.
///
/// The error text is what the renderer surfaces verbatim, so a user who lands on
/// the terminal pane with the setting off is told where the switch is instead of
/// seeing a blank pane.
pub fn gate(settings: &DeviceSettings) -> Result<()> {
    if settings.terminal_enabled {
        return Ok(());
    }
    Err(Error::Other(anyhow::anyhow!(
        "the in-app terminal is disabled on this device — turn it on in Settings › Security before opening a shell"
    )))
}

/// The gate every terminal entry point calls first.
pub fn require_terminal_enabled() -> Result<()> {
    gate(&read_device_settings())
}

/// Read the flag for the settings UI.
pub async fn get_terminal_enabled() -> Result<bool> {
    Ok(read_device_settings().terminal_enabled)
}

/// Set the flag from the settings UI.
///
/// Turning it OFF does not kill sessions that are already open; that is
/// `terminal_close`'s job, and the renderer does it when the pane unmounts. What
/// this guarantees is that no NEW shell starts.
pub async fn set_terminal_enabled(enabled: bool) -> Result<()> {
    let mut settings = read_device_settings();
    settings.terminal_enabled = enabled;
    write_at(&settings_path(), &settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_install_has_no_file_and_therefore_no_shell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-settings.json");
        let settings = read_at(&path);
        assert!(!settings.terminal_enabled);
        assert!(gate(&settings).is_err());
    }

    #[test]
    fn the_flag_round_trips_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-settings.json");

        write_at(&path, &DeviceSettings { terminal_enabled: true }).unwrap();
        assert!(read_at(&path).terminal_enabled);
        assert!(gate(&read_at(&path)).is_ok());

        write_at(&path, &DeviceSettings { terminal_enabled: false }).unwrap();
        assert!(!read_at(&path).terminal_enabled);
        assert!(gate(&read_at(&path)).is_err());
    }

    /// A file we cannot parse must not be read as "on". The whole point of the
    /// default is that only an intact, affirmative file enables a shell.
    #[test]
    fn a_corrupt_settings_file_reads_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-settings.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(!read_at(&path).terminal_enabled);
        assert!(gate(&read_at(&path)).is_err());
    }

    /// A file written by an older build has no `terminal_enabled` key at all.
    /// `serde(default)` has to give that the off answer.
    #[test]
    fn a_file_without_the_key_reads_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-settings.json");
        std::fs::write(&path, b"{}").unwrap();
        assert!(!read_at(&path).terminal_enabled);
    }

    /// The gate is only worth anything if `terminal_open` asks it.
    ///
    /// The decision above is a pure function, so every test of it passes just
    /// as well with nothing calling it — which is exactly the regression this
    /// closes. A source guard rather than a live call because standing up a
    /// real `AppState` (keystore, DB, DS) to open a PTY costs more than the
    /// thing it would prove, and the shape it has to catch is "somebody deletes
    /// the one line". Same pattern as `pollis-relay`'s `http::seam_tests`.
    ///
    /// `terminal_windows.rs` is deliberately NOT checked: every command there
    /// already returns `unsupported()`, so there is no shell for a gate to
    /// stop.
    #[test]
    fn terminal_open_asks_the_gate() {
        let src = include_str!("terminal_unix.rs");
        let body = src
            .split_once("pub async fn terminal_open(")
            .expect("terminal_unix.rs must still define terminal_open")
            .1;
        let opening = body
            .split_once("-> Result<String> {")
            .expect("its signature")
            .1;
        // Only the first 30 lines: the gate has to run BEFORE the PTY is opened
        // and the shell is spawned, not somewhere further down.
        let prelude: String = opening.lines().take(30).collect::<Vec<_>>().join("\n");
        let gate_at = prelude.find("require_terminal_enabled()").expect(
            "terminal_open must call require_terminal_enabled() before it spawns \
             anything; the device-local switch is the only thing standing between \
             the renderer and the user's login shell",
        );
        if let Some(pty_at) = prelude.find("openpty(") {
            assert!(
                gate_at < pty_at,
                "the gate must run before openpty(), not after"
            );
        }
    }

    /// The write path creates the data directory if it is not there yet — a
    /// first-ever toggle on a fresh profile must not fail.
    #[test]
    fn writing_creates_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("device-settings.json");
        write_at(&path, &DeviceSettings { terminal_enabled: true }).unwrap();
        assert!(read_at(&path).terminal_enabled);
    }
}
