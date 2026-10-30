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
}

/// Record the app's config for the extension. Best-effort.
pub fn save_config(data_dir: &Path, config: &Config) {
    let c = HeadlessConfig {
        r2_endpoint: config.r2_endpoint.clone(),
        r2_public_url: config.r2_public_url.clone(),
        livekit_url: config.livekit_url.clone(),
        pollis_delivery_url: config.pollis_delivery_url.clone(),
        dev_access_key: config.dev_access_key.clone(),
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

/// Move `old_dir` to `new_dir` once (#1599).
///
/// Runs only when `new_dir` holds nothing and `old_dir` exists. On one volume this is a single
/// atomic `rename`. Across volumes the tree is copied into a sibling staging directory, fsynced,
/// renamed into place, and only then is the old tree removed, so an interruption leaves the old
/// directory whole and the next launch starts over.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub(crate) fn migrate_dir(old_dir: &Path, new_dir: &Path) -> std::io::Result<()> {
    if old_dir == new_dir || !old_dir.is_dir() || !is_empty_or_missing(new_dir)? {
        return Ok(());
    }
    if let Some(parent) = new_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // An empty placeholder (a host that created the directory) is in the way of a rename.
    if new_dir.exists() {
        std::fs::remove_dir(new_dir)?;
    }
    match std::fs::rename(old_dir, new_dir) {
        Ok(()) => {
            crate::diag!("[storage] moved the data directory to its override location");
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(e) => return Err(e),
    }
    copy_then_swap(old_dir, new_dir)
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn copy_then_swap(old_dir: &Path, new_dir: &Path) -> std::io::Result<()> {
    let staging = new_dir.with_extension("migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_tree(old_dir, &staging)?;
    std::fs::rename(&staging, new_dir)?;
    std::fs::remove_dir_all(old_dir)?;
    crate::diag!("[storage] copied the data directory to its override location");
    Ok(())
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn is_empty_or_missing(dir: &Path) -> std::io::Result<bool> {
    match std::fs::read_dir(dir) {
        Ok(mut it) => Ok(it.next().is_none()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
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
        };
        save_config(t.path(), &config);
        let back = load_config(t.path()).unwrap();
        assert_eq!(back.pollis_delivery_url.as_deref(), Some("https://api-dev.example"));
        assert_eq!(back.r2_public_url, "https://media.example");
        assert!(load_config(&t.path().join("nope")).is_none());
    }
}
