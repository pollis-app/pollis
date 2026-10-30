//! What a headless process needs from the app's environment (#1599): the data dir, the
//! non-secret connection config the app was started with, and (iOS) the one-time move of the
//! data dir into the App Group container.

use std::path::Path;

use crate::config::Config;

/// The config file the app writes at every `init_pollis`, beside the database.
const CONFIG_FILE: &str = "notify-config.json";

/// The subset of [`Config`] a headless fetch needs, as the app last started with it.
///
/// No secret: the DS URL and R2 origins are public. `dev_access_key` is set only in dev builds
/// (it is the dev DS gate key, never sent to prod), and the file is owner-only in the sandbox.
#[derive(serde::Serialize, serde::Deserialize)]
struct HeadlessConfig {
    r2_endpoint: String,
    r2_public_url: String,
    livekit_url: String,
    pollis_delivery_url: Option<String>,
    #[serde(default)]
    dev_access_key: Option<String>,
    /// The app's build (#1611), so the extension's DS requests carry the same version headers.
    #[serde(default)]
    client: Option<crate::client_policy::ClientIdentity>,
}

/// Record the app's config for the extension. Best-effort.
pub fn save_config(data_dir: &Path, config: &Config) {
    let c = HeadlessConfig {
        r2_endpoint: config.r2_endpoint.clone(),
        r2_public_url: config.r2_public_url.clone(),
        livekit_url: config.livekit_url.clone(),
        pollis_delivery_url: config.pollis_delivery_url.clone(),
        dev_access_key: config.dev_access_key.clone(),
        client: config.client.clone(),
    };
    let Ok(json) = serde_json::to_vec(&c) else {
        return;
    };
    if let Err(e) = crate::private_fs::write(&data_dir.join(CONFIG_FILE), json) {
        crate::diag!("[notify] headless config not written: {e}");
    }
}

/// The config the app last started with, or `None` if it never has on this build.
pub fn load_config(data_dir: &Path) -> Option<Config> {
    let raw = std::fs::read(data_dir.join(CONFIG_FILE)).ok()?;
    let c: HeadlessConfig = serde_json::from_slice(&raw).ok()?;
    Some(Config {
        r2_endpoint: c.r2_endpoint,
        r2_public_url: c.r2_public_url,
        livekit_url: c.livekit_url,
        pollis_delivery_url: c.pollis_delivery_url.filter(|s| !s.is_empty()),
        overlay_mode: pollis_relay::OverlayMode::Off,
        overlay_relay_url: None,
        overlay_relay_cert: None,
        overlay_directory_url: None,
        overlay_directory_key: None,
        dev_access_key: c.dev_access_key.filter(|s| !s.trim().is_empty()),
        client: c.client,
        client_policy_url: None,
    })
}

/// Point this process at `data_dir` the way the app's own init would, so every path (and on
/// Android, the keystore namespace) matches the app's. Never migrates anything.
pub fn adopt_data_dir(data_dir: &Path) {
    #[cfg(target_os = "ios")]
    {
        // A relocation, not an explicit dir: the keystore namespace stays unprefixed.
        if crate::db::local::dirs_path() != data_dir {
            crate::db::local::set_data_dir_override(data_dir);
        }
    }
    #[cfg(target_os = "android")]
    {
        // The app exports POLLIS_DATA_DIR at init. In a process the app never started in,
        // the in-process equivalent gives the same paths and the same keystore namespace.
        if crate::db::local::dirs_path() != data_dir {
            crate::db::local::set_data_dir(data_dir);
        }
        if std::env::var_os("SSL_CERT_FILE").is_none() {
            if let Err(e) = crate::android_tls::install(data_dir) {
                crate::diag!("[notify] android tls bootstrap: {e}");
            }
        }
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        if crate::db::local::dirs_path() != data_dir {
            crate::db::local::set_data_dir(data_dir);
        }
    }
}

/// Files only the app's own data dir holds. A directory with none of them holds nothing of the
/// user's: at most a lock or config file some process created before the move.
const APP_DATA_MARKERS: &[&str] = &["keystore.pks", "dev-keystore.json", "accounts.json"];

fn has_app_data(dir: &Path) -> bool {
    APP_DATA_MARKERS.iter().any(|m| dir.join(m).is_file())
}

/// Where a copied-and-swapped old tree is parked until it can be deleted.
fn retired_path(old_dir: &Path) -> std::path::PathBuf {
    old_dir.with_extension("migrated")
}

/// Move `old_dir` to `new_dir` once (#1599).
///
/// - Nothing happens unless `old_dir` holds the app's data and `new_dir` does not. A populated
///   `new_dir` is never overwritten, and a populated `old_dir` beside it is never deleted
///   (it may be newer: a build without the App Group wrote there).
/// - On one volume this is a single atomic `rename`.
/// - Across volumes the tree is copied into a sibling staging directory, fsynced and renamed
///   into place; the old tree is then renamed aside to `<old>.migrated` (atomic) and deleted.
///   An interruption before the swap leaves the old directory whole and the next launch starts
///   over; a `.migrated` leftover is deleted, best-effort, on every later launch.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub fn migrate_dir(old_dir: &Path, new_dir: &Path) -> std::io::Result<()> {
    let retired = retired_path(old_dir);
    if retired.exists() {
        if let Err(e) = std::fs::remove_dir_all(&retired) {
            crate::diag!("[storage] old data directory not removed yet: {e}");
        }
    }
    if old_dir == new_dir || !has_app_data(old_dir) || has_app_data(new_dir) {
        return Ok(());
    }
    if let Some(parent) = new_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // No user data in the way, only what a process made before the move: clear it for the rename.
    if new_dir.exists() {
        std::fs::remove_dir_all(new_dir)?;
    }
    match std::fs::rename(old_dir, new_dir) {
        Ok(()) => {
            crate::diag!("[storage] moved the data directory to its override location");
            return Ok(());
        }
        Err(e) if is_cross_device(&e) => {}
        Err(e) => return Err(e),
    }
    copy_then_swap(old_dir, new_dir)
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn is_cross_device(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn copy_then_swap(old_dir: &Path, new_dir: &Path) -> std::io::Result<()> {
    let staging = new_dir.with_extension("migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_tree(old_dir, &staging)?;
    std::fs::rename(&staging, new_dir)?;
    // The copy is in place; park the original aside (atomic) so it is never mistaken for live
    // data, then delete it. A failed delete is retried on the next launch.
    let retired = retired_path(old_dir);
    std::fs::rename(old_dir, &retired)?;
    if let Err(e) = std::fs::remove_dir_all(&retired) {
        crate::diag!("[storage] old data directory not removed yet: {e}");
    }
    crate::diag!("[storage] copied the data directory to its override location");
    Ok(())
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    crate::private_fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ty.is_file() {
            std::fs::copy(&src, &dst)?;
            std::fs::File::open(&dst)?.sync_all()?;
        }
    }
    std::fs::File::open(to)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populate(dir: &Path) {
        std::fs::create_dir_all(dir.join("media-cache")).unwrap();
        std::fs::write(dir.join("keystore.pks"), b"ks").unwrap();
        std::fs::write(dir.join("pollis_u.db"), b"db").unwrap();
        std::fs::write(dir.join("media-cache/blob"), b"m").unwrap();
    }

    fn assert_moved(new: &Path) {
        assert_eq!(std::fs::read(new.join("keystore.pks")).unwrap(), b"ks");
        assert_eq!(std::fs::read(new.join("pollis_u.db")).unwrap(), b"db");
        assert_eq!(std::fs::read(new.join("media-cache/blob")).unwrap(), b"m");
    }

    #[test]
    fn the_old_dir_moves_into_an_empty_override() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("Application Support/pollis");
        let new = t.path().join("group/pollis");
        populate(&old);
        std::fs::create_dir_all(&new).unwrap();
        migrate_dir(&old, &new).unwrap();
        assert_moved(&new);
        assert!(!old.exists());
    }

    #[test]
    fn a_populated_override_is_never_overwritten() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        let new = t.path().join("new");
        populate(&old);
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("keystore.pks"), b"already here").unwrap();
        migrate_dir(&old, &new).unwrap();
        assert_eq!(std::fs::read(new.join("keystore.pks")).unwrap(), b"already here");
        assert!(old.join("keystore.pks").exists(), "the old tree is left alone");
    }

    #[test]
    fn no_old_dir_is_a_fresh_install() {
        let t = tempfile::tempdir().unwrap();
        migrate_dir(&t.path().join("missing"), &t.path().join("new")).unwrap();
        assert!(!t.path().join("new").exists());
    }

    #[test]
    fn the_cross_volume_path_copies_then_swaps() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        let new = t.path().join("new");
        populate(&old);
        copy_then_swap(&old, &new).unwrap();
        assert_moved(&new);
        assert!(!old.exists());
        assert!(!new.with_extension("migrating").exists());
    }

    #[test]
    fn a_lock_file_made_before_the_move_does_not_block_it() {
        // The extension (or a lock) may have created the override with no user data in it.
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        let new = t.path().join("new");
        populate(&old);
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("state.lock"), b"").unwrap();
        migrate_dir(&old, &new).unwrap();
        assert_moved(&new);
        assert!(!old.exists());
    }

    #[test]
    fn a_leftover_from_a_finished_copy_is_removed_on_a_later_launch() {
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        let new = t.path().join("new");
        populate(&new);
        let leftover = retired_path(&old);
        populate(&leftover);
        migrate_dir(&old, &new).unwrap();
        assert!(!leftover.exists(), "the parked copy is deleted");
        assert_moved(&new);
    }

    #[test]
    fn a_populated_old_dir_beside_a_populated_new_one_is_kept() {
        // Possibly newer data from a build without the App Group: never deleted.
        let t = tempfile::tempdir().unwrap();
        let old = t.path().join("old");
        let new = t.path().join("new");
        populate(&old);
        populate(&new);
        migrate_dir(&old, &new).unwrap();
        assert!(old.join("keystore.pks").exists());
    }

    #[test]
    fn the_config_round_trips_without_overlay_settings() {
        let t = tempfile::tempdir().unwrap();
        let config = Config {
            r2_endpoint: "https://r2.example".into(),
            r2_public_url: "https://media.example".into(),
            livekit_url: String::new(),
            pollis_delivery_url: Some("https://api-dev.example".into()),
            overlay_mode: pollis_relay::OverlayMode::Off,
            overlay_relay_url: None,
            overlay_relay_cert: None,
            overlay_directory_url: None,
            overlay_directory_key: None,
            dev_access_key: None,
            client: None,
            client_policy_url: None,
        };
        save_config(t.path(), &config);
        let back = load_config(t.path()).unwrap();
        assert_eq!(back.pollis_delivery_url.as_deref(), Some("https://api-dev.example"));
        assert_eq!(back.r2_public_url, "https://media.example");
        assert!(load_config(&t.path().join("nope")).is_none());
    }
}
