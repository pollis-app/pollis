use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

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

    fn read_map() -> Result<HashMap<String, String>> {
        let path = store_path();
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(e) => return Err(Error::Keystore(format!("read dev-keystore.json: {e}"))),
        };
        match serde_json::from_str(&data) {
            Ok(m) => Ok(m),
            Err(parse_err) => {
                // Refuse to silently replace a corrupt keystore with an empty
                // one — the next write would erase every stored key for every
                // user on this device. Back it up loud and bail.
                let ts = chrono::Utc::now().timestamp();
                let backup = path.with_file_name(format!("dev-keystore.bad-{ts}.json"));
                if let Err(rename_err) = std::fs::rename(&path, &backup) {
                    eprintln!(
                        "[keystore] failed to rename corrupt dev-keystore.json to {}: {rename_err}",
                        backup.display()
                    );
                }
                eprintln!(
                    "[keystore] dev-keystore.json was corrupt ({parse_err}); backed up to {}",
                    backup.display()
                );
                Err(Error::Keystore(format!(
                    "dev keystore corrupt; backed up to {}",
                    backup.display()
                )))
            }
        }
    }

    fn write_map(map: &HashMap<String, String>) -> Result<()> {
        use std::io::Write;

        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Keystore(format!("create dir: {e}")))?;
        }
        let data = serde_json::to_string(map)
            .map_err(|e| Error::Keystore(format!("serialize: {e}")))?;

        // Atomic write: tempfile + fsync + rename. A crash before the rename
        // leaves the old file intact. Without this, a crash mid-write turned
        // the keystore into a zero-byte file and bounced every user to OTP.
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| Error::Keystore(format!("open dev-keystore.json.tmp: {e}")))?;
            f.write_all(data.as_bytes())
                .map_err(|e| Error::Keystore(format!("write dev-keystore.json.tmp: {e}")))?;
            f.sync_all()
                .map_err(|e| Error::Keystore(format!("fsync dev-keystore.json.tmp: {e}")))?;
        }
        std::fs::rename(&tmp, &path)
            .map_err(|e| Error::Keystore(format!("rename dev-keystore.json.tmp: {e}")))?;
        Ok(())
    }

    pub async fn store(key: &str, value: &[u8]) -> Result<()> {
        let key = namespaced(key);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value);
        tokio::task::spawn_blocking(move || {
            let mut map = read_map()?;
            map.insert(key, encoded);
            write_map(&map)
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn load(key: &str) -> Result<Option<Vec<u8>>> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let map = read_map()?;
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
            let mut map = read_map()?;
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

// ── Trait abstraction (production + in-memory test impls) ────────────────────

/// Abstraction over secret storage. Production uses [`OsKeystore`] which wraps
/// the OS keychain (release) or a JSON file under the data dir (debug). Tests
/// use [`InMemoryKeystore`] so every [`TestClient`] gets its own isolated
/// keystore without touching real user credentials.
#[async_trait]
pub trait Keystore: Send + Sync {
    async fn store(&self, key: &str, value: &[u8]) -> Result<()>;
    async fn load(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn delete(&self, key: &str) -> Result<()>;

    async fn store_for_user(&self, key: &str, user_id: &str, value: &[u8]) -> Result<()> {
        self.store(&format!("{key}_{user_id}"), value).await
    }

    async fn load_for_user(&self, key: &str, user_id: &str) -> Result<Option<Vec<u8>>> {
        self.load(&format!("{key}_{user_id}")).await
    }

    async fn delete_for_user(&self, key: &str, user_id: &str) -> Result<()> {
        self.delete(&format!("{key}_{user_id}")).await
    }
}

/// Production keystore. Byte-for-byte identical to the pre-trait behaviour —
/// this is a thin delegation to the existing `backend` module which writes to
/// the OS keychain in release builds and a JSON file in debug.
pub struct OsKeystore;

#[async_trait]
impl Keystore for OsKeystore {
    async fn store(&self, key: &str, value: &[u8]) -> Result<()> {
        backend::store(key, value).await
    }

    async fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
        backend::load(key).await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        backend::delete(key).await
    }
}

/// In-memory keystore for tests. Each [`TestClient`] gets its own instance so
/// multiple simulated users can coexist in a single test process without their
/// account identity keys or session tokens colliding. Never use in production.
pub struct InMemoryKeystore {
    inner: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl InMemoryKeystore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryKeystore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Keystore for InMemoryKeystore {
    async fn store(&self, key: &str, value: &[u8]) -> Result<()> {
        self.inner.lock().await.insert(key.to_string(), value.to_vec());
        Ok(())
    }

    async fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.inner.lock().await.get(key).cloned())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.inner.lock().await.remove(key);
        Ok(())
    }
}

/// Convenience: construct the default production keystore wrapped in the Arc
/// expected by [`AppState::keystore`].
pub fn default_os_keystore() -> Arc<dyn Keystore> {
    Arc::new(OsKeystore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_keystore_roundtrip() {
        let ks = InMemoryKeystore::new();
        ks.store("k1", b"v1").await.unwrap();
        assert_eq!(ks.load("k1").await.unwrap().as_deref(), Some(&b"v1"[..]));
        ks.delete("k1").await.unwrap();
        assert!(ks.load("k1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_keystore_per_user_namespacing() {
        let ks = InMemoryKeystore::new();
        ks.store_for_user("session", "alice", b"a").await.unwrap();
        ks.store_for_user("session", "bob", b"b").await.unwrap();
        assert_eq!(ks.load_for_user("session", "alice").await.unwrap().as_deref(), Some(&b"a"[..]));
        assert_eq!(ks.load_for_user("session", "bob").await.unwrap().as_deref(), Some(&b"b"[..]));
    }
}
