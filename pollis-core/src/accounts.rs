use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::db::local::dirs_path;
use crate::error::{Error, Result};

/// One entry in the on-disk `accounts.json`.
///
/// Every field this code SETS is either an opaque identifier or a display string
/// the user chose: no secret, and — since #997 moved the login address into the
/// per-user keystore — no PII. The one exception is [`AccountInfo::legacy_email`],
/// which is only ever read off a file an older build wrote, carried until the
/// keystore has taken it, and then erased.
///
/// No new field may go in without `#[serde(default)]`. A missing field on an
/// un-annotated type is a serde parse ERROR, and [`read_accounts_index`] treats
/// a parse error as corruption: it would rename every existing install's index
/// to `accounts.bad-<ts>.json`, minting exactly the permanent plaintext-email
/// snapshot #997 exists to eliminate. Removing a field is always safe in the
/// other direction — serde ignores unknown keys on read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub user_id: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub last_seen: String,
    /// The pre-#997 plaintext login address, present only on a file written by
    /// an older build. Read solely so the upgrade can move it into the keystore
    /// (`commands::auth::read_accounts_index_migrated`) and then erase it via
    /// [`forget_legacy_emails`].
    ///
    /// `default` per the rule above — current files have no such key.
    /// `skip_serializing_if` so that erasing it is something a caller has to
    /// DO — clear the field — rather than something any write does implicitly.
    /// That distinction is the invariant: an ordinary `upsert_account` must not
    /// be able to destroy an address the keystore has not taken yet, which is
    /// precisely how it would be lost for good.
    #[serde(default, rename = "email", skip_serializing_if = "Option::is_none")]
    pub legacy_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountsIndex {
    pub accounts: Vec<AccountInfo>,
    pub last_active_user: Option<String>,
}

/// One entry as the LOGIN SCREEN sees it: an [`AccountInfo`] rehydrated with the
/// login address, which since #997 lives in the per-user keystore rather than on
/// disk. Byte-for-byte the JSON shape `list_known_accounts` returned before that
/// move, so the renderer's `AccountInfo` type is unchanged.
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

/// How many `accounts.bad-*.json` snapshots to keep. The read path writes one
/// per corruption and nothing ever removed them, so they accumulated for the
/// life of the install.
const MAX_BAD_INDEX_BACKUPS: usize = 3;

fn index_path() -> PathBuf {
    dirs_path().join("accounts.json")
}

/// Read the accounts index.
///
/// - Missing file → `Ok(default)`. First run.
/// - Parse failure → rename the bad file to `accounts.bad-<unix-ms>.json`
///   and return `AccountsIndexCorrupt`. We refuse to silently replace a
///   corrupt index with an empty one because the next `upsert_account`
///   would then overwrite it with a single-entry file, permanently
///   losing the record of every other account on this device.
///
///   The stamp is in MILLISECONDS. It used to be seconds, so two corruptions
///   inside the same second chose the same name and the second rename silently
///   destroyed the first snapshot. Snapshots are pruned to the newest
///   [`MAX_BAD_INDEX_BACKUPS`] by the next successful write.
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
        crate::private_fs::create_dir_all(parent)
            .map_err(|e| Error::Other(anyhow::anyhow!("create accounts dir: {e}")))?;
    }
    let data = serde_json::to_string_pretty(index)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize accounts: {e}")))?;

    let tmp = path.with_extension("json.tmp");
    {
        // Owner-only from creation: this file holds the user's real email
        // address, and `rename` carries the temp file's mode onto the target.
        let mut f = crate::private_fs::create_file(&tmp)
            .map_err(|e| Error::Other(anyhow::anyhow!("open accounts.json.tmp: {e}")))?;
        f.write_all(data.as_bytes())
            .map_err(|e| Error::Other(anyhow::anyhow!("write accounts.json.tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| Error::Other(anyhow::anyhow!("fsync accounts.json.tmp: {e}")))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        // The old index survives — that is the point of the rename — but the
        // freshly-written `.tmp` does not, and nothing else ever collected it.
        // Remove it so a failed write leaves the directory as it found it.
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
/// Best-effort throughout: this runs after a write that already succeeded, and
/// failing to tidy an old snapshot must never turn that write into an error.
///
/// Newest-last is decided by file name, which orders chronologically across
/// both stamp widths this code has written: a millisecond stamp is its own
/// second stamp's digits plus three more, so a legacy `accounts.bad-<sec>.json`
/// sorts exactly where its instant falls among the millisecond ones.
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
            eprintln!("[accounts] could not prune {}: {e}", stale.display());
        }
    }
}

/// Insert or update an account entry and set it as the last active user.
///
/// Takes no email. The login address moved to the per-user keystore in #997;
/// callers that have one store it there via
/// [`crate::commands::auth::store_login_email`] alongside this call.
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
    ///
    /// Touches nothing else: not `avatar_url`, not `last_seen`, not
    /// `last_active_user` — this is a rename, not a sign-in.
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
/// `get_session` reads the username from this file, so a rename that only
/// reached the Delivery Service came back as the OLD name on the next launch
/// — and as the sender name on every optimistic row until the send resolved
/// against the remote row. An account the index has never seen is ignored.
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
        // Promote the most-recently-seen remaining account, or None.
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
/// The ONLY thing that removes the plaintext address from disk, and the reason
/// [`AccountInfo::legacy_email`] survives an ordinary write: this runs strictly
/// AFTER the caller has confirmed the keystore holds every one of those
/// addresses (see `commands::auth::read_accounts_index_migrated`), so a keystore
/// that refused the write leaves the file intact for the next attempt.
///
/// A no-op on a file that carries no legacy field, so a normal launch neither
/// rewrites nor re-fsyncs anything.
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

    /// The upgrade path for #997. Every install that ever logged in has an
    /// `accounts.json` carrying the now-removed `email` key, and `AccountInfo`
    /// annotates no field with `#[serde(default)]` — so a shape mismatch here is
    /// not a lenient no-op, it is a parse error, which `read_accounts_index`
    /// escalates to `AccountsIndexCorrupt` and a permanent
    /// `accounts.bad-<ts>.json` copy of exactly the plaintext email this change
    /// exists to remove.
    ///
    /// Dropping a field is safe (serde ignores unknown keys); ADDING one is not.
    /// That asymmetry is the invariant this test pins.
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

        // The address is surfaced, once, so the upgrade can move it to the
        // keystore instead of losing the returning-device resolution with it.
        assert_eq!(
            index.accounts[0].legacy_email.as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(index.accounts[1].legacy_email, None);
    }

    /// A file written by the CURRENT build has no `email` key at all, and must
    /// parse just as cleanly — `#[serde(default)]` is what makes the field's
    /// absence legal rather than corruption.
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

    /// The invariant that makes the migration safe to interrupt: an ordinary
    /// write — `upsert_account`, `remove_account`, `clear_last_active_user` —
    /// PRESERVES a legacy address it has not been told to erase.
    ///
    /// If a plain write dropped it, a single login on a multi-account device
    /// would destroy the other accounts' addresses before the keystore had ever
    /// seen them, and nothing could recover them. Erasure has to be a deliberate
    /// act, which is what `forget_legacy_emails` is.
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

    /// The erase step itself: clearing the field and writing leaves no `email`
    /// key and no address behind.
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

    /// The email must not come back on the way OUT either: re-serializing an
    /// index a legacy file was read from writes no `email` key, so the first
    /// write after upgrade is what actually erases the address from disk.
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

    /// A rename changes the username and nothing else — in particular not the
    /// avatar and not which account is active, which `upsert_account` would
    /// both clobber.
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

        // Same name again, and an unknown account: nothing to write.
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

        // Six snapshots, oldest first, plus two files the prune must not touch.
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
