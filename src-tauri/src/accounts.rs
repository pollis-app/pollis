use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::db::local::dirs_path;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub last_seen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountsIndex {
    pub accounts: Vec<AccountInfo>,
    pub last_active_user: Option<String>,
}

fn index_path() -> PathBuf {
    dirs_path().join("accounts.json")
}

/// Read the accounts index.
///
/// - Missing file → `Ok(default)`. First run.
/// - Parse failure → rename the bad file to `accounts.bad-<unix-ts>.json`
///   and return `AccountsIndexCorrupt`. We refuse to silently replace a
///   corrupt index with an empty one because the next `upsert_account`
///   would then overwrite it with a single-entry file, permanently
///   losing the record of every other account on this device.
pub fn read_accounts_index() -> Result<AccountsIndex> {
    let path = index_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AccountsIndex::default());
        }
        Err(e) => {
            return Err(Error::Other(anyhow::anyhow!(
                "read accounts.json: {e}"
            )));
        }
    };

    match serde_json::from_str::<AccountsIndex>(&data) {
        Ok(idx) => Ok(idx),
        Err(parse_err) => {
            let ts = chrono::Utc::now().timestamp();
            let backup = path.with_file_name(format!("accounts.bad-{ts}.json"));
            if let Err(rename_err) = std::fs::rename(&path, &backup) {
                eprintln!(
                    "[accounts] failed to rename corrupt index to {}: {rename_err}",
                    backup.display()
                );
            }
            eprintln!(
                "[accounts] accounts.json was corrupt ({parse_err}); backed up to {}",
                backup.display()
            );
            Err(Error::AccountsIndexCorrupt {
                backup_path: backup.to_string_lossy().into_owned(),
            })
        }
    }
}

/// Atomic write: serialize to a sibling `.tmp` file, fsync, then rename
/// over the target. POSIX rename is atomic; Windows `MoveFileEx` with
/// replace-existing (which `std::fs::rename` uses on recent Rust) is
/// atomic on NTFS. A crash before the rename leaves the old file intact.
fn write_accounts_index(index: &AccountsIndex) -> Result<()> {
    use std::io::Write;

    let path = index_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Other(anyhow::anyhow!("create accounts dir: {e}")))?;
    }
    let data = serde_json::to_string_pretty(index)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize accounts: {e}")))?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| Error::Other(anyhow::anyhow!("open accounts.json.tmp: {e}")))?;
        f.write_all(data.as_bytes())
            .map_err(|e| Error::Other(anyhow::anyhow!("write accounts.json.tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| Error::Other(anyhow::anyhow!("fsync accounts.json.tmp: {e}")))?;
    }
    std::fs::rename(&tmp, &path)
        .map_err(|e| Error::Other(anyhow::anyhow!("rename accounts.json.tmp: {e}")))?;
    Ok(())
}

/// Insert or update an account entry and set it as the last active user.
pub fn upsert_account(user_id: &str, username: &str, email: Option<&str>, avatar_url: Option<&str>) -> Result<()> {
    let mut index = read_accounts_index()?;

    let now = chrono::Utc::now().to_rfc3339();
    if let Some(existing) = index.accounts.iter_mut().find(|a| a.user_id == user_id) {
        existing.username = username.to_string();
        if let Some(e) = email {
            existing.email = Some(e.to_string());
        }
        existing.avatar_url = avatar_url.map(|s| s.to_string());
        existing.last_seen = now;
    } else {
        index.accounts.push(AccountInfo {
            user_id: user_id.to_string(),
            username: username.to_string(),
            email: email.map(|s| s.to_string()),
            avatar_url: avatar_url.map(|s| s.to_string()),
            last_seen: now,
        });
    }
    index.last_active_user = Some(user_id.to_string());

    write_accounts_index(&index)
}

/// Remove an account from the index (on delete_data logout).
pub fn remove_account(user_id: &str) -> Result<()> {
    let mut index = read_accounts_index().unwrap_or_default();
    index.accounts.retain(|a| a.user_id != user_id);
    if index.last_active_user.as_deref() == Some(user_id) {
        // Promote the most-recently-seen remaining account, or None.
        index.last_active_user = index
            .accounts
            .iter()
            .max_by_key(|a| a.last_seen.as_str())
            .map(|a| a.user_id.clone());
    }
    write_accounts_index(&index)
}

/// Clear the last active user (soft logout — account entry stays in the list).
pub fn clear_last_active_user() -> Result<()> {
    let mut index = read_accounts_index().unwrap_or_default();
    index.last_active_user = None;
    write_accounts_index(&index)
}
