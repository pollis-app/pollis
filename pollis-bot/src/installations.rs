//! The installations this runtime hosts: one record each at
//! `<data dir>/installations/<installation_id>/installation.json`. Its
//! encrypted store is `<data dir>/stores/<device id>/` (`core_host`) and its
//! keys sit in its own keystore slot. Nothing is shared between two installations.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::app_approval::ArtifactLocation;
use crate::app_handler::AppHandler;
use crate::handler::Handler;

pub const INSTALLATIONS_DIR: &str = "installations";
pub const RECORD_FILE: &str = "installation.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationRecord {
    /// The DS claim scope. The bot's user id while a bot has one installation.
    pub installation_id: String,
    pub bot_user_id: String,
    pub group_id: String,
    /// This host's device id under the bot.
    pub device_id: String,
    /// The App this installation runs. `None` until `pollis-bot app install`:
    /// such an installation does not run, and `pollis-bot status` says so.
    #[serde(default)]
    pub app: Option<InstalledApp>,
    pub created_at: String,
}

/// Where an installation's App is: its component and `pollis-app.json`, each a
/// local path or an `https://` URL. It runs only under an admin's verified
/// approval (#2005), re-checked before every job; a development build's
/// signature is read from `<component>.sig.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledApp {
    pub component: ArtifactLocation,
    pub manifest: ArtifactLocation,
}

impl InstalledApp {
    pub fn handler(&self) -> Arc<dyn Handler> {
        Arc::new(AppHandler::approved(self.component.clone(), self.manifest.clone()))
    }
}

/// What to do about an installation with no App, for logs, `status` and `post`.
pub fn no_app_fix(installation_id: &str) -> String {
    format!("run `pollis-bot app install --installation {installation_id} --artifact <directory or https:// URL>`, then restart `pollis-bot run`")
}

/// Ids are ULIDs; anything else is refused before it can name a path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn new(data_dir: &Path) -> Self {
        Self { root: data_dir.join(INSTALLATIONS_DIR) }
    }

    pub fn dir_for(&self, installation_id: &str) -> Result<PathBuf> {
        if !valid_id(installation_id) {
            bail!("`{installation_id}` is not an installation id");
        }
        Ok(self.root.join(installation_id))
    }

    /// Every installation on this host, by id. A directory without a readable
    /// record is reported, not skipped: a half-written pairing should be seen.
    pub fn list(&self) -> Result<Vec<InstallationRecord>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.root.display())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join(RECORD_FILE);
            let bytes = std::fs::read(&path).with_context(|| {
                format!("{} is missing: remove the directory if its pairing never finished", path.display())
            })?;
            let record: InstallationRecord = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a valid installation record", path.display()))?;
            if entry.file_name().to_string_lossy() != record.installation_id {
                bail!("{} names a different installation than its directory", path.display());
            }
            records.push(record);
        }
        records.sort_by(|a, b| a.installation_id.cmp(&b.installation_id));
        Ok(records)
    }

    pub fn get(&self, installation_id: &str) -> Result<Option<InstallationRecord>> {
        Ok(self.list()?.into_iter().find(|r| r.installation_id == installation_id))
    }

    /// Write a record atomically (temp file + rename).
    pub fn save(&self, record: &InstallationRecord) -> Result<()> {
        let dir = self.dir_for(&record.installation_id)?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = dir.join(format!("{RECORD_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
        std::fs::rename(&tmp, dir.join(RECORD_FILE))?;
        Ok(())
    }
}

/// The installations-per-runtime limit, refused with its fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverCapacity {
    pub count: usize,
    pub max: usize,
}

impl std::fmt::Display for OverCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this runtime is configured for at most {} installations and would host {}. Fix: raise `max_installations` in config.json if this machine can carry more, or run the extra bots on another host",
            self.max, self.count
        )
    }
}

impl std::error::Error for OverCapacity {}

/// `count` installations fit under `max`.
pub fn check_capacity(count: usize, max: usize) -> Result<(), OverCapacity> {
    if count > max {
        return Err(OverCapacity { count, max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> InstallationRecord {
        InstallationRecord {
            installation_id: id.into(),
            bot_user_id: id.into(),
            group_id: "01GROUP".into(),
            device_id: "01DEVICE".into(),
            app: None,
            created_at: "2026-11-18T00:00:00Z".into(),
        }
    }

    #[test]
    fn records_round_trip_and_list_in_id_order() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        assert!(reg.list().unwrap().is_empty());
        reg.save(&record("01B")).unwrap();
        reg.save(&record("01A")).unwrap();
        let ids: Vec<_> = reg.list().unwrap().into_iter().map(|r| r.installation_id).collect();
        assert_eq!(ids, ["01A", "01B"]);
        assert_eq!(reg.get("01B").unwrap(), Some(record("01B")));
    }

    #[test]
    fn a_path_like_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path());
        for id in ["../x", "a/b", "", "a.b"] {
            assert!(reg.dir_for(id).is_err(), "{id}");
        }
    }

    #[test]
    fn capacity_allows_up_to_the_max_and_refuses_past_it() {
        assert!(check_capacity(8, 8).is_ok());
        let e = check_capacity(9, 8).unwrap_err();
        assert!(e.to_string().contains("max_installations"));
    }
}
