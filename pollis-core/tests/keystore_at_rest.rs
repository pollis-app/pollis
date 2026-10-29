//! #882/#950 — the production keystore, end to end, against a real data directory.
//!
//! One test in phases: the data dir is process-wide, so tests would race for it.

use pollis_core::keystore::{backend_kind, slot_name, BackendKind, Keystore, OsKeystore};

const STORE: &str = "keystore.pks";
const LEGACY_STORE: &str = "dev-keystore.json";

#[tokio::test]
async fn the_production_keystore_never_leaves_plaintext_in_the_data_dir() {
    let dir = std::env::temp_dir().join(format!("pollis-ks-e2e-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    pollis_core::db::local::set_data_dir(&dir);

    let kind = backend_kind().await;
    assert!(
        matches!(kind, BackendKind::OsKeychain | BackendKind::EncryptedFile),
        "no third, plaintext backend may exist"
    );

    if kind == BackendKind::OsKeychain {
        // Don't write junk into the developer's real keychain.
        assert!(
            !dir.join(STORE).exists(),
            "the keychain backend must not write a keystore file"
        );
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    let ks = OsKeystore;

    // Phase 1: a plaintext pre-#882 store under the pre-#950 name
    let legacy_secret = b"a-pre-882-account-identity-key";
    let legacy_slot = slot_name("account_id_key_wrapped", "legacy_user");
    let legacy_json = format!(
        "{{\"{legacy_slot}\":\"{}\"}}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, legacy_secret)
    );
    std::fs::write(dir.join(LEGACY_STORE), &legacy_json).unwrap();

    assert_eq!(
        ks.load_for_user("account_id_key_wrapped", "legacy_user")
            .await
            .unwrap()
            .as_deref(),
        Some(&legacy_secret[..]),
        "a pre-#882 plaintext store under the pre-#950 name must still be readable"
    );
    assert!(
        !dir.join(LEGACY_STORE).exists(),
        "the old name must be gone once the new file is written and verified"
    );
    assert!(dir.join(STORE).exists(), "the store must be at its real name");
    let raw = std::fs::read(dir.join(STORE)).unwrap();
    assert_eq!(
        &raw[..4],
        b"PKS\x01",
        "the migrated file must be sealed, not the plaintext it came from"
    );
    assert!(
        !raw.windows(legacy_secret.len()).any(|w| w == legacy_secret),
        "the migrated secret must not be readable on disk"
    );

    // Phase 2: ordinary store / load / delete at rest
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

    let raw = std::fs::read(dir.join(STORE)).unwrap();
    assert_eq!(&raw[..4], b"PKS\x01", "the store must be in the sealed format");
    assert!(
        !raw.windows(secret.len()).any(|w| w == secret),
        "the stored secret must not appear in the file"
    );
    assert!(
        String::from_utf8_lossy(&raw).find("account_id_key_wrapped").is_none(),
        "the slot name must not appear in the file"
    );
    assert_eq!(
        ks.load_for_user("account_id_key_wrapped", "legacy_user")
            .await
            .unwrap()
            .as_deref(),
        Some(&legacy_secret[..]),
        "the migrated key must survive subsequent writes"
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
