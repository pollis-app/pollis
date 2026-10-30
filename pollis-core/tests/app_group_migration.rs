//! #1599: a signed-in install survives the move of its data directory into the iOS App Group
//! container, end to end on the host: the same keystore slots, the same accounts index, the
//! same PIN unlocking the same database. Its own process, because the data dir is global.
//!
//! What this cannot cover is the iOS Keychain half (the KEK sealing `keystore.pks` on a phone);
//! that is `keystore::kek_plan`, and on a device the app-only item stays readable.

use std::sync::Arc;

use pollis_core::commands::{auth, pin};
use pollis_core::config::Config;
use pollis_core::state::AppState;

const USER: &str = "01JTMIGRATEUSER00000000000";
const DEVICE: &str = "01JTMIGRATEDEVICE000000000";
const PIN: &str = "1357";

fn app(keystore: Arc<dyn pollis_core::keystore::Keystore>) -> Arc<AppState> {
    // No DS: the session restore must not depend on one (the probe fails open).
    let config = Config {
        r2_endpoint: String::new(),
        r2_public_url: String::new(),
        livekit_url: String::new(),
        pollis_delivery_url: None,
        overlay_mode: pollis_relay::OverlayMode::Off,
        overlay_relay_url: None,
        overlay_relay_cert: None,
        overlay_directory_url: None,
        overlay_directory_key: None,
        dev_access_key: None,
        client: None,
        client_policy_url: None,
    };
    Arc::new(AppState::new_with_parts(config, keystore))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_in_install_survives_the_app_group_move() {
    let root = tempfile::tempdir().expect("tempdir");
    let old = root.path().join("Library/Application Support/pollis");
    let new = root.path().join("Shared/AppGroup/pollis");

    // The build before #1599: data in the old directory, signed in, PIN set.
    pollis_core::set_data_dir_override(&old);
    let keystore = pollis_core::keystore::default_os_keystore();
    let before = app(keystore.clone());
    keystore.store_for_user("device_id", USER, DEVICE.as_bytes()).await.unwrap();
    // The pre-PIN slots `set_pin` wraps on first use.
    keystore.store_for_user("db_key", USER, &[7u8; 32]).await.unwrap();
    keystore.store_for_user("account_id_key", USER, &[42u8; 32]).await.unwrap();
    pollis_core::accounts::upsert_account(USER, "mia", None).unwrap();
    pin::set_pin(&before, None, PIN.to_string()).await.expect("set_pin");
    {
        let guard = before.local_db.lock().await;
        guard
            .as_ref()
            .expect("db open after set_pin")
            .conn()
            .execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
                 VALUES ('m1', 'c1', 'u2', X'00', 'kept across the move', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
    }
    pin::lock(&before).await.unwrap();
    drop(before);

    // The #1599 build: the host relocates the data dir, init moves the old tree once.
    pollis_core::notify::host::migrate_dir(&old, &new).expect("migrate");
    pollis_core::set_data_dir_override(&new);
    assert!(!old.exists(), "the old tree moved");
    assert!(new.join("keystore.pks").is_file() || new.join("dev-keystore.json").is_file());

    let after = app(pollis_core::keystore::default_os_keystore());
    let snap = pin::get_unlock_state(&after).await.expect("unlock state");
    assert_eq!(snap.last_active_user.as_deref(), Some(USER), "the account is still there");
    assert!(snap.pin_set, "the keystore slots are read under the same namespace");

    let session = auth::get_session(&after).await.expect("get_session");
    assert_eq!(session.map(|p| p.id).as_deref(), Some(USER), "the session restores");

    pin::unlock(&after, USER.to_string(), PIN.to_string())
        .await
        .expect("the same PIN unlocks the moved database");
    let guard = after.local_db.lock().await;
    let kept: String = guard
        .as_ref()
        .expect("db open")
        .conn()
        .query_row("SELECT content FROM message WHERE id = 'm1'", [], |r| r.get(0))
        .expect("history survived");
    assert_eq!(kept, "kept across the move");
}
