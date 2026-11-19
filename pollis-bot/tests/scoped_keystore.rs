//! Two runtimes on one machine never share a keystore slot, and a person's PIN
//! slot stays visible to the core's unattended-path refusal.

use std::sync::Arc;

use pollis_bot::core_host::ScopedKeystore;
use pollis_core::keystore::{InMemoryKeystore, Keystore};

#[tokio::test]
async fn data_dirs_get_separate_slots_and_pin_meta_reads_through() {
    let os: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let host_a = ScopedKeystore::new(os.clone(), a.path());
    let host_b = ScopedKeystore::new(os.clone(), b.path());

    host_a.store_for_user("bot_installation", "01BOT", b"host a").await.unwrap();
    host_b.store_for_user("bot_installation", "01BOT", b"host b").await.unwrap();
    assert_eq!(host_a.load_for_user("bot_installation", "01BOT").await.unwrap().unwrap(), b"host a");
    assert_eq!(host_b.load_for_user("bot_installation", "01BOT").await.unwrap().unwrap(), b"host b");
    assert!(os.load_for_user("bot_installation", "01BOT").await.unwrap().is_none(), "nothing unscoped is written");

    host_a.delete_for_user("bot_installation", "01BOT").await.unwrap();
    assert!(host_a.load_for_user("bot_installation", "01BOT").await.unwrap().is_none());
    assert!(host_b.load_for_user("bot_installation", "01BOT").await.unwrap().is_some());

    os.store_for_user("pin_meta", "01HUMAN", b"pin").await.unwrap();
    assert!(host_a.load_for_user("pin_meta", "01HUMAN").await.unwrap().is_some());
}
