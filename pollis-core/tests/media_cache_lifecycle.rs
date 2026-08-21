//! The media-cache wipe, driven through the four commands that own it.
//!
//! The cache holds every image, video and voice clip the user opened, decrypted
//! on the way in and re-encrypted under the session's `db_key`. Three commands
//! promise to empty it — `logout`, `set_pin`, `unlock` — and a fourth,
//! `wipe_local_data`, promises to leave nothing on the computer at all. None of
//! them did.
//!
//! * The three lifecycle callers resolved the directory from ambient state
//!   (`CURRENT_CACHE_USER`) and every one of them called at a moment when that
//!   state named the wrong user: `logout` after `unload_user_db` had cleared it,
//!   `set_pin` and `unlock` before `load_user_db_with_key` had set it. All three
//!   wiped `media-cache/_anon/`, which is empty, and reported success.
//! * `wipe_local_data` removed `db::local::dirs_path()`, while the cache root is
//!   the shell's `app_data_dir().join("media-cache")`. Those are different
//!   directories on Linux and Windows (only macOS makes them coincide), so
//!   "wipe this computer" left the entire cache behind.
//!
//! Ambient state is what made all four wrong, so the fix is a scope argument and
//! this is the test that the argument is right at every call site. It seeds a
//! real file in the real cache directory, runs the real command, and looks.
//!
//! Its own test binary, and one test in phases rather than four: the data
//! directory and the cache root are both process-global, so pointing them
//! somewhere is a whole-process act — and phase 4 deletes the data directory.

use std::sync::Arc;

use pollis_core::commands::r2;
use pollis_core::config::Config;
use pollis_core::keystore::InMemoryKeystore;
use pollis_core::state::AppState;

const USER: &str = "01JCACHEUSERLIFECYCLE0000";
const PIN: &str = "8241";

fn config() -> Config {
    Config {
        r2_endpoint: String::new(),
        r2_public_url: String::new(),
        livekit_url: String::new(),
        // No Delivery Service: every command below is local, and the
        // best-effort DS calls at the tail of `set_pin` / `unlock` fail fast
        // and are logged rather than propagated. That is the shipped behaviour
        // for an offline client, so the test drives the same code an offline
        // user does.
        pollis_delivery_url: None,
        overlay_mode: pollis_relay::OverlayMode::Off,
        overlay_relay_url: None,
        overlay_relay_cert: None,
        overlay_directory_url: None,
        overlay_directory_key: None,
    }
}

/// Put a file where the cache keeps this user's media, and hand back its path.
fn seed_cache_entry(root: &std::path::Path, tag: &str) -> std::path::PathBuf {
    let dir = root.join(USER);
    std::fs::create_dir_all(&dir).expect("create user cache dir");
    let path = dir.join(format!("{tag}.png.enc"));
    std::fs::write(&path, b"an image the user opened").expect("seed cache entry");
    assert!(path.exists());
    path
}

#[tokio::test]
async fn every_lifecycle_wipe_actually_empties_this_user_s_media_cache() {
    let scratch = std::env::temp_dir().join(format!("pollis-cache-lifecycle-{}", ulid::Ulid::new()));
    let data_dir = scratch.join("data");
    let cache_root = scratch.join("media-cache");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    std::fs::create_dir_all(&cache_root).expect("cache root");
    pollis_core::db::local::set_data_dir(&data_dir);
    r2::set_media_cache_dir(cache_root.clone());

    let state = Arc::new(AppState::new_with_parts(
        config(),
        Arc::new(InMemoryKeystore::new()),
    ));

    // A known account, as `verify_otp` would have left one.
    pollis_core::accounts::upsert_account(USER, "cache-tester", Some("t@example.com"), None)
        .expect("seed accounts index");

    // ── Phase 1: set_pin ─────────────────────────────────────────────────
    // The key material `set_pin` wraps comes from `AppState.unlock`, exactly as
    // it does after signup.
    *state.unlock.lock().await = Some(pollis_core::commands::pin::UnlockState {
        user_id: USER.to_string(),
        db_key: zeroize::Zeroizing::new(vec![3u8; 32]),
        account_id_key: zeroize::Zeroizing::new(vec![4u8; 32]),
    });
    let seeded = seed_cache_entry(&cache_root, "set-pin");
    pollis_core::commands::pin::set_pin(&state, None, PIN.to_string())
        .await
        .expect("set_pin");
    assert!(
        !seeded.exists(),
        "set_pin left this user's cached media on disk"
    );

    // ── Phase 2: unlock ──────────────────────────────────────────────────
    pollis_core::commands::pin::lock(&state).await.expect("lock");
    let seeded = seed_cache_entry(&cache_root, "unlock");
    pollis_core::commands::pin::unlock(&state, USER.to_string(), PIN.to_string())
        .await
        .expect("unlock");
    assert!(
        !seeded.exists(),
        "unlock left this user's cached media on disk"
    );

    // ── Phase 3: logout ──────────────────────────────────────────────────
    // `delete_data: false` — the session ends, the account stays known. The
    // cache must go either way.
    let seeded = seed_cache_entry(&cache_root, "logout");
    pollis_core::commands::auth::logout(&state, false)
        .await
        .expect("logout");
    assert!(
        !seeded.exists(),
        "logout left this user's cached media on disk"
    );

    // ── Phase 4: wipe_local_data ─────────────────────────────────────────
    // The one that was looking at the wrong ROOT, not merely the wrong user —
    // so this phase asserts on a second user's directory too.
    let mine = seed_cache_entry(&cache_root, "wipe");
    let theirs = cache_root.join("someone-else").join("ffff.png.enc");
    std::fs::create_dir_all(theirs.parent().unwrap()).expect("other user dir");
    std::fs::write(&theirs, b"another account's media").expect("seed other user");

    pollis_core::commands::auth::wipe_local_data(&state)
        .await
        .expect("wipe_local_data");

    assert!(
        !mine.exists(),
        "\"wipe this computer\" left the media cache behind — it removes the data \
         directory, and the cache root is somewhere else entirely"
    );
    assert!(
        !theirs.exists(),
        "\"wipe this computer\" left another account's cached media behind"
    );
    assert!(
        !data_dir.exists(),
        "the data directory itself should be gone"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
