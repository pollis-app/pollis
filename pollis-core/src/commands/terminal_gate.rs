//! The switch that has to be thrown before the in-app terminal exists.
//! Off unless the user enabled it; a missing or unreadable file means off.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::local::dirs_path;
use crate::error::{Error, Result};

/// Device-local settings that must be decided in Rust rather than in the
/// renderer. Every field's `Default` must be the safe answer that grants nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSettings {
    /// Whether the in-app terminal pane may spawn a shell on this device.
    #[serde(default)]
    pub terminal_enabled: bool,
}

fn settings_path() -> PathBuf {
    dirs_path().join("device-settings.json")
}

/// Read settings out of `path`, failing safe.
/// Missing, unreadable or unparseable all read as [`DeviceSettings::default`].
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

/// The gate as a pure function; its error text is shown verbatim by the renderer.
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

    #[test]
    fn a_corrupt_settings_file_reads_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-settings.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(!read_at(&path).terminal_enabled);
        assert!(gate(&read_at(&path)).is_err());
    }

    #[test]
    fn a_file_without_the_key_reads_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-settings.json");
        std::fs::write(&path, b"{}").unwrap();
        assert!(!read_at(&path).terminal_enabled);
    }

    /// The gate is only worth anything if `terminal_open` asks it.
    /// Source guard: catches deletion of the one gating line.
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

    #[test]
    fn writing_creates_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("device-settings.json");
        write_at(&path, &DeviceSettings { terminal_enabled: true }).unwrap();
        assert!(read_at(&path).terminal_enabled);
    }
}
