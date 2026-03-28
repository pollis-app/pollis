use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::db::local::dirs_path;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: String,
    pub username: String,
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

pub fn read_accounts_index() -> AccountsIndex {
    let path = index_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return AccountsIndex::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn write_accounts_index(index: &AccountsIndex) -> Result<()> {
    let path = index_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create accounts dir: {e}")))?;
    }
    let data = serde_json::to_string_pretty(index)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("serialize accounts: {e}")))?;
    std::fs::write(&path, data)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("write accounts.json: {e}")))?;
    Ok(())
}

/// Insert or update an account entry and set it as the last active user.
pub fn upsert_account(user_id: &str, username: &str, avatar_url: Option<&str>) -> Result<()> {
    let mut index = read_accounts_index();

    let now = chrono::Utc::now().to_rfc3339();
    if let Some(existing) = index.accounts.iter_mut().find(|a| a.user_id == user_id) {
        existing.username = username.to_string();
        existing.avatar_url = avatar_url.map(|s| s.to_string());
        existing.last_seen = now;
    } else {
        index.accounts.push(AccountInfo {
            user_id: user_id.to_string(),
            username: username.to_string(),
            avatar_url: avatar_url.map(|s| s.to_string()),
            last_seen: now,
        });
    }
    index.last_active_user = Some(user_id.to_string());

    write_accounts_index(&index)
}

/// Remove an account from the index (on delete_data logout).
pub fn remove_account(user_id: &str) -> Result<()> {
    let mut index = read_accounts_index();
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
    let mut index = read_accounts_index();
    index.last_active_user = None;
    write_accounts_index(&index)
}
