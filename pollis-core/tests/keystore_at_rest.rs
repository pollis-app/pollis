//! #882 — the production keystore, end to end, against a real data directory.
//!
//! The unit tests in `keystore.rs` drive the codec and the file operations with
//! an explicit machine secret. This one goes through the actual shipped type —
//! `OsKeystore`, the same value `AppState` holds — with `POLLIS_DATA_DIR`
//! pointed at a temp dir, and then reads the bytes that ended up on disk.
//!
//! It lives in its own test binary precisely so it can set `POLLIS_DATA_DIR`
//! for the whole process without racing another test that reads it.

use pollis_core::keystore::{backend_kind, BackendKind, Keystore, OsKeystore};

#[tokio::test]
async fn the_production_keystore_never_leaves_plaintext_in_the_data_dir() {
    let dir = std::env::temp_dir().join(format!("pollis-ks-e2e-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("POLLIS_DATA_DIR", &dir);

    let kind = backend_kind().await;
    assert!(
        matches!(kind, BackendKind::OsKeychain | BackendKind::EncryptedFile),
        "no third, plaintext backend may exist"
    );

    if kind == BackendKind::OsKeychain {
        // A release build on a host that has a credential store. Writing here
        // would put junk in the developer's real keychain, so this case stops
        // at the property it can check without side effects: the keychain
        // backend does not touch the data dir at all.
        assert!(
            !dir.join("dev-keystore.json").exists(),
            "the keychain backend must not write a keystore file"
        );
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    let ks = OsKeystore;
    let secret = b"an-account-identity-private-key";
    ks.store_for_user("account_id_key_wrapped", "u1", secret)
        .await
        .unwrap();

    assert_eq!(
        ks.load_for_user("account_id_key_wrapped", "u1")
            .await
            .unwrap()
            .as_deref(),
        Some(&secret[..])
    );

    // The bytes that actually landed on disk.
    let raw = std::fs::read(dir.join("dev-keystore.json")).unwrap();
    assert_eq!(&raw[..4], b"PKS\x01", "the store must be in the sealed format");
    assert!(
        !raw.windows(secret.len()).any(|w| w == secret),
        "the stored secret must not appear in the file"
    );
    assert!(
        String::from_utf8_lossy(&raw).find("account_id_key_wrapped").is_none(),
        "the slot name must not appear in the file"
    );

    ks.delete_for_user("account_id_key_wrapped", "u1")
        .await
        .unwrap();
    assert!(ks
        .load_for_user("account_id_key_wrapped", "u1")
        .await
        .unwrap()
        .is_none());

    std::fs::remove_dir_all(&dir).ok();
}
