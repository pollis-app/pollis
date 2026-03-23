use crate::error::{Error, Result};

/// When POLLIS_DATA_DIR is set (second dev instance), namespace keyring entries
/// so multiple instances don't stomp each other's session/identity keys.
/// Production builds without POLLIS_DATA_DIR are unaffected.
fn namespaced(key: &str) -> String {
    #[cfg(debug_assertions)]
    let key = format!("DEV:{key}");

    match std::env::var("POLLIS_DATA_DIR") {
        Ok(dir) => {
            let label = std::path::Path::new(&dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("dev2");
            format!("{label}:{key}")
        }
        Err(_) => key.to_string(),
    }
}

// ── Debug builds: plain JSON file (no keychain, no OS prompts) ──────────────

#[cfg(debug_assertions)]
mod backend {
    use super::namespaced;
    use crate::error::{Error, Result};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn store_path() -> PathBuf {
        #[cfg(target_os = "macos")]
        let base = {
            if let Ok(dir) = std::env::var("POLLIS_DATA_DIR") {
                PathBuf::from(dir)
            } else {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join("Library/Application Support/com.pollis.app")
            }
        };
        #[cfg(target_os = "linux")]
        let base = {
            if let Ok(dir) = std::env::var("POLLIS_DATA_DIR") {
                PathBuf::from(dir)
            } else {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".local/share/pollis")
            }
        };
        #[cfg(target_os = "windows")]
        let base = {
            if let Ok(dir) = std::env::var("POLLIS_DATA_DIR") {
                PathBuf::from(dir)
            } else {
                let appdata = std::env::var("APPDATA").unwrap_or_default();
                PathBuf::from(appdata).join("pollis")
            }
        };
        base.join("dev-keystore.json")
    }

    fn read_map() -> HashMap<String, String> {
        let path = store_path();
        let Ok(data) = std::fs::read_to_string(&path) else { return HashMap::new() };
        serde_json::from_str(&data).unwrap_or_default()
    }

    fn write_map(map: &HashMap<String, String>) -> Result<()> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Keystore(format!("create dir: {e}")))?;
        }
        let data = serde_json::to_string(map)
            .map_err(|e| Error::Keystore(format!("serialize: {e}")))?;
        std::fs::write(&path, data)
            .map_err(|e| Error::Keystore(format!("write: {e}")))?;
        Ok(())
    }

    pub async fn store(key: &str, value: &[u8]) -> Result<()> {
        let key = namespaced(key);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value);
        tokio::task::spawn_blocking(move || {
            let mut map = read_map();
            map.insert(key, encoded);
            write_map(&map)
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn load(key: &str) -> Result<Option<Vec<u8>>> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let map = read_map();
            match map.get(&key) {
                None => Ok(None),
                Some(encoded) => {
                    let bytes = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        encoded,
                    )
                    .map_err(|e| Error::Keystore(format!("base64 decode: {e}")))?;
                    Ok(Some(bytes))
                }
            }
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn delete(key: &str) -> Result<()> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let mut map = read_map();
            map.remove(&key);
            write_map(&map)
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }
}

// ── Release builds: OS keychain ──────────────────────────────────────────────

#[cfg(not(debug_assertions))]
mod backend {
    use super::namespaced;
    use crate::error::{Error, Result};
    use keyring::Entry;

    const SERVICE: &str = "pollis";

    pub async fn store(key: &str, value: &[u8]) -> Result<()> {
        let key = namespaced(key);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value);
        tokio::task::spawn_blocking(move || {
            let entry = Entry::new(SERVICE, &key)
                .map_err(|e| Error::Keystore(e.to_string()))?;
            entry.set_password(&encoded)
                .map_err(|e| Error::Keystore(e.to_string()))
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn load(key: &str) -> Result<Option<Vec<u8>>> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let entry = Entry::new(SERVICE, &key)
                .map_err(|e| Error::Keystore(e.to_string()))?;
            match entry.get_password() {
                Ok(encoded) => {
                    let bytes = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        encoded,
                    )
                    .map_err(|e| Error::Keystore(format!("base64 decode: {e}")))?;
                    Ok(Some(bytes))
                }
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(Error::Keystore(e.to_string())),
            }
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn delete(key: &str) -> Result<()> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let entry = Entry::new(SERVICE, &key)
                .map_err(|e| Error::Keystore(e.to_string()))?;
            entry.delete_credential()
                .map_err(|e| Error::Keystore(e.to_string()))
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }
}

// ── Public API (delegates to the active backend) ─────────────────────────────

pub async fn store(key: &str, value: &[u8]) -> Result<()> {
    backend::store(key, value).await
}

pub async fn load(key: &str) -> Result<Option<Vec<u8>>> {
    backend::load(key).await
}

pub async fn delete(key: &str) -> Result<()> {
    backend::delete(key).await
}
