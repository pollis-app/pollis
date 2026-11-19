//! What `pollis-bot status` reads: the running runtime writes
//! `<data dir>/status.json` whenever an installation's state changes (never on
//! a timer).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STATUS_FILE: &str = "status.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    Starting,
    CatchingUp,
    /// Caught up and waiting for the next trigger.
    Connected,
    /// The last job failed; `last_error` and `fix` say why and what to do.
    JobFailed,
}

impl InstallationState {
    pub fn label(self) -> &'static str {
        match self {
            InstallationState::Starting => "starting",
            InstallationState::CatchingUp => "catching up",
            InstallationState::Connected => "connected",
            InstallationState::JobFailed => "job failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationStatus {
    pub state: InstallationState,
    pub handler: String,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatusFile {
    pub pid: u32,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_bind: Option<String>,
    pub installations: BTreeMap<String, InstallationStatus>,
}

impl StatusFile {
    pub fn read(data_dir: &Path) -> Option<StatusFile> {
        let bytes = std::fs::read(data_dir.join(STATUS_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// The running runtime's view, written through on every change.
pub struct StatusBoard {
    path: Option<PathBuf>,
    file: std::sync::Mutex<StatusFile>,
}

impl StatusBoard {
    pub fn new(data_dir: Option<&Path>, webhook_bind: Option<String>) -> Self {
        let file = StatusFile {
            pid: std::process::id(),
            started_at: now(),
            webhook_bind,
            installations: BTreeMap::new(),
        };
        let board = Self { path: data_dir.map(|d| d.join(STATUS_FILE)), file: std::sync::Mutex::new(file) };
        board.flush();
        board
    }

    pub fn set(&self, installation_id: &str, handler: &str, state: InstallationState, error: Option<(String, String)>) {
        {
            let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
            let (last_error, fix) = match error {
                Some((e, f)) => (Some(e), Some(f)),
                None => (None, None),
            };
            file.installations.insert(
                installation_id.to_string(),
                InstallationStatus { state, handler: handler.to_string(), since: now(), last_error, fix },
            );
        }
        self.flush();
    }

    pub fn snapshot(&self) -> StatusFile {
        self.file.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn flush(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let bytes = match serde_json::to_vec_pretty(&self.snapshot()) {
            Ok(b) => b,
            Err(_) => return,
        };
        let tmp = path.with_extension("json.tmp");
        // Status is advisory: a failed write is logged, never fatal.
        if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)) {
            crate::log::failure("runtime", format!("writing {}: {e}", path.display()), "check the data directory is writable");
        }
    }
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
