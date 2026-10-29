use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::db::local::dirs_path;
use crate::error::{Error, Result};

/// One entry in the on-disk `accounts.json`.
///
/// Fields are opaque ids or user-chosen display strings: no secret or PII (#997), except [`AccountInfo::legacy_email`].
///
/// New fields need `#[serde(default)]`: a parse error is treated as corruption and snapshots the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub last_seen: String,
    /// Pre-#997 plaintext login address, read only so the upgrade can move it to the keystore.
    /// `skip_serializing_if` so erasing it takes an explicit clear; an ordinary write must not lose it.
    #[serde(default, rename = "email", skip_serializing_if = "Option::is_none")]
    pub legacy_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountsIndex {
    pub accounts: Vec<AccountInfo>,
    pub last_active_user: Option<String>,
}

/// An [`AccountInfo`] rehydrated with the keystore login address; same JSON shape as before #997.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownAccount {
    pub user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub last_seen: String,
}

/// The wire shape of `list_known_accounts` — see [`KnownAccount`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnownAccounts {
    pub accounts: Vec<KnownAccount>,
    pub last_active_user: Option<String>,
}

/// How many `accounts.bad-*.json` snapshots to keep.
const MAX_BAD_INDEX_BACKUPS: usize = 3;

fn index_path() -> PathBuf {
    dirs_path().join("accounts.json")
}

/// Read the accounts index.
///
/// - Missing file → `Ok(default)`. First run.
/// - Parse failure → rename to `accounts.bad-<unix-ms>.json`, return `AccountsIndexCorrupt`; never replace with empty.
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
            let ts = chrono::Utc::now().timestamp_millis();
            let backup = path.with_file_name(format!("accounts.bad-{ts}.json"));
            if let Err(rename_err) = std::fs::rename(&path, &backup) {
                crate::diag!(
                    "[accounts] failed to rename corrupt index to {}: {rename_err}",
                    backup.display()
                );
            }
            crate::diag!(
                "[accounts] accounts.json was corrupt ({parse_err}); backed up to {}",
                backup.display()
            );
            Err(Error::AccountsIndexCorrupt {
                backup_path: backup.to_string_lossy().into_owned(),
            })
        }
    }
}

/// Atomic write: `.tmp`, fsync, rename. A crash before the rename leaves the old file intact.
fn write_accounts_index(index: &AccountsIndex) -> Result<()> {
    use std::io::Write;

    let path = index_path();
    if let Some(parent) = path.parent() {
        crate::private_fs::create_dir_all(parent)
            .map_err(|e| Error::Other(anyhow::anyhow!("create accounts dir: {e}")))?;
    }
    let data = serde_json::to_string_pretty(index)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize accounts: {e}")))?;

    let tmp = path.with_extension("json.tmp");
    {
        // Owner-only from creation; `rename` carries the temp file's mode.
        let mut f = crate::private_fs::create_file(&tmp)
            .map_err(|e| Error::Other(anyhow::anyhow!("open accounts.json.tmp: {e}")))?;
        f.write_all(data.as_bytes())
            .map_err(|e| Error::Other(anyhow::anyhow!("write accounts.json.tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| Error::Other(anyhow::anyhow!("fsync accounts.json.tmp: {e}")))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        // Remove the orphaned `.tmp` so a failed write leaves the directory as it found it.
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Other(anyhow::anyhow!("rename accounts.json.tmp: {e}")));
    }

    if let Some(parent) = path.parent() {
        prune_bad_index_backups(parent);
    }
    Ok(())
}

/// Keep only the newest [`MAX_BAD_INDEX_BACKUPS`] `accounts.bad-*.json`
/// snapshots and delete the rest.
///
/// Best-effort. File-name order is chronological across both sec and ms stamp widths.
fn prune_bad_index_backups(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut backups: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("accounts.bad-") && name.ends_with(".json"))
        })
        .collect();

    if backups.len() <= MAX_BAD_INDEX_BACKUPS {
        return;
    }

    backups.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for stale in backups.iter().skip(MAX_BAD_INDEX_BACKUPS) {
        if let Err(e) = std::fs::remove_file(stale) {
            crate::diag!("[accounts] could not prune {}: {e}", stale.display());
        }
    }
}

/// Insert or update an account entry and set it as the last active user.
///
/// Takes no email; callers store it via [`crate::commands::auth::store_login_email`] (#997).
pub fn upsert_account(user_id: &str, username: &str, avatar_url: Option<&str>) -> Result<()> {
    let mut index = read_accounts_index()?;

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
            legacy_email: None,
        });
    }
    index.last_active_user = Some(user_id.to_string());

    write_accounts_index(&index)
}

impl AccountsIndex {
    /// Set the display username of one account. Returns whether it changed.
    pub fn rename(&mut self, user_id: &str, username: &str) -> bool {
        match self.accounts.iter_mut().find(|a| a.user_id == user_id) {
            Some(account) if account.username != username => {
                account.username = username.to_string();
                true
            }
            _ => false,
        }
    }
}

/// Record a username change for an account that is already in the index.
///
/// `get_session` reads the username from here. An unknown account is ignored.
pub fn rename_account(user_id: &str, username: &str) -> Result<()> {
    let mut index = read_accounts_index()?;
    if !index.rename(user_id, username) {
        return Ok(());
    }
    write_accounts_index(&index)
}

/// Remove an account from the index (on delete_data logout).
pub fn remove_account(user_id: &str) -> Result<()> {
    let mut index = read_accounts_index().unwrap_or_default();
    index.accounts.retain(|a| a.user_id != user_id);
    if index.last_active_user.as_deref() == Some(user_id) {
        index.last_active_user = index
            .accounts
            .iter()
            .max_by_key(|a| a.last_seen.as_str())
            .map(|a| a.user_id.clone());
    }
    write_accounts_index(&index)
}

/// Erase every pre-#997 `email` field from the index.
///
/// Runs only AFTER the keystore holds every address, so a refused write leaves the file for a retry.
///
/// A no-op when no legacy field exists.
pub fn forget_legacy_emails() -> Result<()> {
    let mut index = read_accounts_index()?;
    if index.accounts.iter().all(|a| a.legacy_email.is_none()) {
        return Ok(());
    }
    for account in index.accounts.iter_mut() {
        account.legacy_email = None;
    }
    write_accounts_index(&index)
}

/// Clear the last active user (soft logout — account entry stays in the list).
pub fn clear_last_active_user() -> Result<()> {
    let mut index = read_accounts_index().unwrap_or_default();
    index.last_active_user = None;
    write_accounts_index(&index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #997 upgrade path: an old `accounts.json` with `email` must parse, not be flagged corrupt.
    #[test]
    fn a_pre_997_index_still_parses() {
        let legacy = r#"{
          "accounts": [
            {
              "user_id": "01JBQ0000000000000000000AA",
              "username": "alice",
              "email": "alice@example.com",
              "avatar_url": null,
              "last_seen": "2026-08-01T10:00:00+00:00"
            },
            {
              "user_id": "01JBQ0000000000000000000BB",
              "username": "bob",
              "email": null,
              "avatar_url": "https://example.com/b.png",
              "last_seen": "2026-08-02T10:00:00+00:00"
            }
          ],
          "last_active_user": "01JBQ0000000000000000000AA"
        }"#;

        let index: AccountsIndex =
            serde_json::from_str(legacy).expect("a pre-#997 accounts.json must still parse");

        assert_eq!(index.accounts.len(), 2);
        assert_eq!(index.accounts[0].username, "alice");
        assert_eq!(index.accounts[1].avatar_url.as_deref(), Some("https://example.com/b.png"));
        assert_eq!(
            index.last_active_user.as_deref(),
            Some("01JBQ0000000000000000000AA")
        );

        // Surfaced once, so the upgrade can move it to the keystore.
        assert_eq!(
            index.accounts[0].legacy_email.as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(index.accounts[1].legacy_email, None);
    }

    /// A current-build file has no `email` key and must parse too.
    #[test]
    fn a_post_997_index_parses_without_an_email_key() {
        let current = r#"{
          "accounts": [
            {
              "user_id": "01JBQ0000000000000000000AA",
              "username": "alice",
              "avatar_url": null,
              "last_seen": "2026-08-01T10:00:00+00:00"
            }
          ],
          "last_active_user": "01JBQ0000000000000000000AA"
        }"#;

        let index: AccountsIndex = serde_json::from_str(current).expect("current file must parse");
        assert_eq!(index.accounts.len(), 1);
        assert_eq!(index.accounts[0].legacy_email, None);
    }

    fn legacy_file() -> &'static str {
        r#"{
          "accounts": [
            {
              "user_id": "01JBQ0000000000000000000AA",
              "username": "alice",
              "email": "alice@example.com",
              "avatar_url": null,
              "last_seen": "2026-08-01T10:00:00+00:00"
            }
          ],
          "last_active_user": "01JBQ0000000000000000000AA"
        }"#
    }

    /// An ordinary write PRESERVES a legacy address it was not told to erase.
    #[test]
    fn an_ordinary_write_preserves_an_unadopted_address() {
        let index: AccountsIndex = serde_json::from_str(legacy_file()).expect("parse");

        let rewritten = serde_json::to_string(&index).expect("serialize");
        let reread: AccountsIndex = serde_json::from_str(&rewritten).expect("re-parse");

        assert_eq!(
            reread.accounts[0].legacy_email.as_deref(),
            Some("alice@example.com"),
            "a write nobody asked to erase must not erase: {rewritten}"
        );
    }

    /// Clearing the field and writing leaves no `email` key.
    #[test]
    fn clearing_the_legacy_field_erases_the_address() {
        let mut index: AccountsIndex = serde_json::from_str(legacy_file()).expect("parse");
        for account in index.accounts.iter_mut() {
            account.legacy_email = None;
        }

        let rewritten = serde_json::to_string(&index).expect("serialize");
        assert!(
            !rewritten.contains("alice@example.com") && !rewritten.contains("email"),
            "the erase must remove the address and its key: {rewritten}"
        );

        let reread: AccountsIndex = serde_json::from_str(&rewritten).expect("re-parse");
        assert_eq!(reread.accounts[0].legacy_email, None);
        assert_eq!(reread.accounts[0].username, "alice");
    }

    #[test]
    fn writing_the_index_emits_no_email_key() {
        let index = AccountsIndex {
            accounts: vec![AccountInfo {
                user_id: "01JBQ0000000000000000000AA".to_string(),
                username: "alice".to_string(),
                avatar_url: None,
                last_seen: "2026-08-01T10:00:00+00:00".to_string(),
                legacy_email: None,
            }],
            last_active_user: Some("01JBQ0000000000000000000AA".to_string()),
        };

        let json = serde_json::to_string(&index).expect("serialize");
        assert!(!json.contains("email"), "accounts.json must carry no email: {json}");
    }

    /// A rename touches neither the avatar nor the active account.
    #[test]
    fn rename_changes_only_the_username() {
        let mut index = AccountsIndex {
            accounts: vec![
                AccountInfo {
                    user_id: "01JBQ0000000000000000000AA".to_string(),
                    username: "alice_x7q2".to_string(),
                    avatar_url: Some("avatars/aa".to_string()),
                    last_seen: "2026-08-01T10:00:00+00:00".to_string(),
                    legacy_email: None,
                },
                AccountInfo {
                    user_id: "01JBQ0000000000000000000BB".to_string(),
                    username: "bob".to_string(),
                    avatar_url: None,
                    last_seen: "2026-08-02T10:00:00+00:00".to_string(),
                    legacy_email: None,
                },
            ],
            last_active_user: Some("01JBQ0000000000000000000BB".to_string()),
        };

        assert!(index.rename("01JBQ0000000000000000000AA", "alice"));
        assert_eq!(index.accounts[0].username, "alice");
        assert_eq!(index.accounts[0].avatar_url.as_deref(), Some("avatars/aa"));
        assert_eq!(index.accounts[0].last_seen, "2026-08-01T10:00:00+00:00");
        assert_eq!(index.accounts[1].username, "bob");
        assert_eq!(index.last_active_user.as_deref(), Some("01JBQ0000000000000000000BB"));

        assert!(!index.rename("01JBQ0000000000000000000AA", "alice"));
        assert!(!index.rename("01JBQ0000000000000000000ZZ", "zed"));
        assert_eq!(index.accounts.len(), 2);
    }

    #[test]
    fn bad_index_backups_are_pruned_to_the_newest_few() {
        let dir = tempfile::Builder::new()
            .prefix("pollis-accounts-prune")
            .tempdir()
            .expect("tempdir");

        let stamps = [
            "1700000000",
            "1755720000000",
            "1755720000001",
            "1755720001000",
            "1755720002000",
            "1755720003000",
        ];
        for stamp in stamps {
            std::fs::write(dir.path().join(format!("accounts.bad-{stamp}.json")), "{}")
                .expect("write backup");
        }
        std::fs::write(dir.path().join("accounts.json"), "{}").expect("write index");
        std::fs::write(dir.path().join("pollis_u.db"), "x").expect("write db");

        prune_bad_index_backups(dir.path());

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();

        assert_eq!(
            left,
            vec![
                "accounts.bad-1755720001000.json".to_string(),
                "accounts.bad-1755720002000.json".to_string(),
                "accounts.bad-1755720003000.json".to_string(),
                "accounts.json".to_string(),
                "pollis_u.db".to_string(),
            ]
        );
    }
}
