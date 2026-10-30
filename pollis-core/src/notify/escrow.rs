//! The notification setting and the DB-key escrow it controls (#1599). SECURITY-RELEVANT.
//!
//! The local database key exists only wrapped under the PIN (`commands::pin`), so a process that
//! starts without the user — a Notification Service Extension, an FCM data-push handler — cannot
//! open the database or decrypt anything. To show who wrote and what, the key must be reachable
//! without the PIN. On MOBILE ONLY, while the user's notification setting is not "Nothing",
//! this module keeps an unwrapped copy of the key in the device keystore under
//! [`ESCROW_SLOT`]. The keystore there is a file sealed by an OS-held key (iOS Keychain
//! `AfterFirstUnlockThisDeviceOnly`, Android Keystore), so the copy is as reachable as the
//! keystore itself: after the phone's first unlock, to code running as this app.
//!
//! The trade-off: with the escrow present the PIN no longer protects the local database from
//! someone who can run code as the app on the unlocked device (Signal's model). Setting
//! "Nothing" deletes the escrow and restores the PIN-only property. Desktop never escrows.
//!
//! Every path that writes or deletes the escrow is in this file:
//! - [`sync_escrow`]: after unlock / set-PIN, and when the setting changes.
//! - [`delete_escrow`]: setting "Nothing", logout, account deletion, wipe, PIN-lockout nuke.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keystore::Keystore;
use crate::state::AppState;

/// The per-user keystore slot holding the unwrapped DB key.
pub const ESCROW_SLOT: &str = "notify_db_key";

/// The `ui_state` key holding the setting (device-local; it is about this phone's lock screen).
pub const MODE_KEY: &str = "notification_content";

/// What a rich notification may show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationContent {
    /// Sender and message text. The default.
    Full,
    /// Sender only.
    Name,
    /// Today's generic text; no escrow.
    None,
}

impl NotificationContent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Name => "name",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Self::Full),
            "name" => Some(Self::Name),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Whether this setting keeps a key escrow.
    fn wants_escrow(self) -> bool {
        self != Self::None
    }
}

/// The stored setting; absent or unreadable is the default, [`NotificationContent::Full`].
pub fn read_mode(conn: &rusqlite::Connection) -> NotificationContent {
    conn.query_row(
        "SELECT value FROM ui_state WHERE key = ?1",
        rusqlite::params![MODE_KEY],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|v| NotificationContent::parse(&v))
    .unwrap_or(NotificationContent::Full)
}

fn write_mode(conn: &rusqlite::Connection, mode: NotificationContent) -> Result<()> {
    conn.execute(
        "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        rusqlite::params![MODE_KEY, mode.as_str()],
    )?;
    Ok(())
}

/// Whether this build may escrow at all: mobile only. Tests turn it on explicitly to exercise
/// the mobile paths on a desktop host.
pub fn escrow_supported() -> bool {
    if cfg!(any(target_os = "ios", target_os = "android")) {
        return true;
    }
    #[cfg(any(test, feature = "test-harness"))]
    {
        if test_support::FORCE_SUPPORTED.with(|f| f.get()) {
            return true;
        }
    }
    false
}

/// Bring the escrow in line with `mode`: present (holding `db_key`) iff escrow is supported and
/// the setting shows anything; absent otherwise.
pub async fn sync_escrow(
    keystore: &dyn Keystore,
    user_id: &str,
    db_key: &[u8],
    mode: NotificationContent,
) -> Result<()> {
    if escrow_supported() && mode.wants_escrow() {
        keystore.store_for_user(ESCROW_SLOT, user_id, db_key).await
    } else {
        delete_escrow(keystore, user_id).await
    }
}

/// Remove the escrow. Idempotent; a missing slot is success.
pub async fn delete_escrow(keystore: &dyn Keystore, user_id: &str) -> Result<()> {
    match keystore.delete_for_user(ESCROW_SLOT, user_id).await {
        Ok(()) => Ok(()),
        Err(e) => match keystore.load_for_user(ESCROW_SLOT, user_id).await {
            // Some backends report deleting a missing entry as an error.
            Ok(None) => Ok(()),
            _ => Err(e),
        },
    }
}

/// The escrowed key, if any. Read by the headless fetch only.
pub async fn load_escrow(
    keystore: &dyn Keystore,
    user_id: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    if !escrow_supported() {
        return Ok(None);
    }
    Ok(keystore
        .load_for_user(ESCROW_SLOT, user_id)
        .await?
        .map(Zeroizing::new))
}

/// After the local DB is open under `db_key` (unlock, set-PIN): write or remove the escrow per
/// the stored setting. Best-effort for the caller: a failure costs rich notifications, never the
/// unlock.
pub async fn refresh_after_unlock(state: &Arc<AppState>, user_id: &str, db_key: &[u8]) {
    let mode = {
        let guard = state.local_db.lock().await;
        match guard.as_ref() {
            Some(db) => read_mode(db.conn()),
            None => return,
        }
    };
    if let Err(e) = sync_escrow(state.keystore.as_ref(), user_id, db_key, mode).await {
        crate::diag!("[notify] notification key escrow not updated: {e}");
    }
}

/// `get_notification_content`: the setting, as `"full" | "name" | "none"`.
pub async fn get_notification_content(state: &Arc<AppState>) -> Result<NotificationContent> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    Ok(read_mode(db.conn()))
}

/// `set_notification_content`: store the setting and write or delete the escrow to match.
/// Needs the unlocked session (the escrow is a copy of its key).
pub async fn set_notification_content(state: &Arc<AppState>, mode: &str) -> Result<()> {
    let mode = NotificationContent::parse(mode).ok_or_else(|| {
        Error::Other(anyhow::anyhow!("notification content must be full, name or none"))
    })?;
    let (user_id, db_key) = {
        let unlock = state.unlock.lock().await;
        let u = unlock
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not unlocked")))?;
        (u.user_id.clone(), u.db_key.clone())
    };
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        write_mode(db.conn(), mode)?;
    }
    sync_escrow(state.keystore.as_ref(), &user_id, &db_key, mode).await
}

#[cfg(any(test, feature = "test-harness"))]
pub mod test_support {
    use std::cell::Cell;

    thread_local! {
        /// Per test thread, so a forced test cannot leak into a "desktop never escrows" one.
        pub(super) static FORCE_SUPPORTED: Cell<bool> = const { Cell::new(false) };
    }

    /// Treat this thread as a mobile build for escrow purposes. Use with a current-thread runtime.
    pub fn force_escrow_supported(on: bool) {
        FORCE_SUPPORTED.with(|f| f.set(on));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::InMemoryKeystore;

    const USER: &str = "01USERESCROW0000000000000A";
    const KEY: &[u8] = &[7u8; 32];

    async fn escrowed(ks: &InMemoryKeystore) -> bool {
        ks.load_for_user(ESCROW_SLOT, USER).await.unwrap().is_some()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn desktop_never_escrows() {
        test_support::force_escrow_supported(false);
        if cfg!(any(target_os = "ios", target_os = "android")) {
            return;
        }
        assert!(!escrow_supported());
        let ks = InMemoryKeystore::new();
        for mode in [NotificationContent::Full, NotificationContent::Name] {
            sync_escrow(&ks, USER, KEY, mode).await.unwrap();
            assert!(!escrowed(&ks).await, "{mode:?} must not escrow on desktop");
        }
        // Even a slot written some other way is never read back on desktop.
        ks.store_for_user(ESCROW_SLOT, USER, KEY).await.unwrap();
        assert!(load_escrow(&ks, USER).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mobile_escrows_for_full_and_name_and_deletes_for_none() {
        test_support::force_escrow_supported(true);
        let ks = InMemoryKeystore::new();
        sync_escrow(&ks, USER, KEY, NotificationContent::Full).await.unwrap();
        assert_eq!(load_escrow(&ks, USER).await.unwrap().unwrap().as_slice(), KEY);
        sync_escrow(&ks, USER, KEY, NotificationContent::Name).await.unwrap();
        assert!(escrowed(&ks).await);
        sync_escrow(&ks, USER, KEY, NotificationContent::None).await.unwrap();
        assert!(!escrowed(&ks).await, "Nothing deletes the escrow");
        test_support::force_escrow_supported(false);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deleting_a_missing_escrow_succeeds() {
        let ks = InMemoryKeystore::new();
        delete_escrow(&ks, USER).await.unwrap();
    }

    #[test]
    fn the_setting_defaults_to_full_and_round_trips() {
        let db = crate::db::local::LocalDb::open_in_memory().unwrap();
        assert_eq!(read_mode(db.conn()), NotificationContent::Full);
        for m in [NotificationContent::Name, NotificationContent::None, NotificationContent::Full] {
            write_mode(db.conn(), m).unwrap();
            assert_eq!(read_mode(db.conn()), m);
        }
        db.conn()
            .execute(
                "UPDATE ui_state SET value = 'garbage' WHERE key = ?1",
                rusqlite::params![MODE_KEY],
            )
            .unwrap();
        assert_eq!(read_mode(db.conn()), NotificationContent::Full);
    }
}
