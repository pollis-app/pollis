//! #882/#950 — the production keystore, end to end, against a real data directory.
//!
//! The unit tests in `keystore.rs` drive the codec and the file operations with
//! an explicit machine secret. This one goes through the actual shipped type —
//! `OsKeystore`, the same value `AppState` holds — with the process data dir
//! pointed at a temp dir, and then reads the bytes that ended up on disk.
//!
//! It lives in its own test binary because the data dir is process-wide: pointing
//! it somewhere is a whole-process act, so it gets a process of its own. That is
//! also why everything below is one `#[tokio::test]` in phases rather than
//! several tests — two tests here would race for the same directory.
//!
//! Note the data dir is set with `db::local::set_data_dir`, NOT
//! `std::env::set_var`: mutating the environment while other threads may be
//! reading it is a data race, which is why Rust 2024 makes `set_var` unsafe
//! (see the note on `db::local`). Nothing in this file needs it.

use pollis_core::keystore::{backend_kind, slot_name, BackendKind, Keystore, OsKeystore};

/// The store's name since #950. `dev-keystore.json` was a fossil: after #882
/// this file is the production store on headless installs and its bytes are
/// AES-256-GCM ciphertext, so both halves of the old name were false.
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
        // A release build on a host that has a credential store. Writing here
        // would put junk in the developer's real keychain, so this case stops
        // at the property it can check without side effects: the keychain
        // backend does not touch the data dir at all.
        assert!(
            !dir.join(STORE).exists(),
            "the keychain backend must not write a keystore file"
        );
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    let ks = OsKeystore;

    // ── Phase 1: an install that predates BOTH #882 and #950 ────────────────
    //
    // Plaintext JSON, under the old name. The shipped type must read it, seal
    // it, and move it to the real name — in that order and without losing the
    // key. This is the whole upgrade path in one step, driven through the
    // public API rather than the module internals.
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

    // ── Phase 2: ordinary store / load / delete at rest ─────────────────────
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
    // The phase-1 key is still there — a later write must not have dropped it.
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
