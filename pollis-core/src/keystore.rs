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

// ── Android: encrypt the keystore file at rest under a hardware-backed key ───
//
// The "file-backed" store below is plaintext on disk. On Android that file
// lives in the app sandbox — readable by root / a compromised backup. Here we
// wrap its bytes with AES-256-GCM under a NON-EXPORTABLE key held in the
// Android Keystore (hardware-backed / StrongBox where available). The key is
// system-managed and persists across app relaunches, so identity + session
// survive a relaunch (issue #185 Section A) while the on-disk ciphertext is
// useless without the device.
//
// Reaches only JVM *system* classes (java.security.*, javax.crypto.*), which
// are loadable from any thread's bootstrap classloader — so this needs no
// Android `Context` and no app classloader, just a `JavaVM` captured in
// JNI_OnLoad. Secrets never cross the RN JS bridge (matches desktop).
#[cfg(target_os = "android")]
mod android_kek {
    use crate::error::{Error, Result};
    use jni::objects::{JByteArray, JObject, JValue};
    use jni::{JNIEnv, JavaVM};
    use std::sync::OnceLock;

    static VM: OnceLock<JavaVM> = OnceLock::new();
    const ALIAS: &str = "pollis_keystore_kek";
    const GCM_TAG_BITS: i32 = 128;
    const IV_LEN: usize = 12;
    // KeyProperties.PURPOSE_ENCRYPT (1) | PURPOSE_DECRYPT (2)
    const PURPOSE_ENCRYPT_DECRYPT: i32 = 3;
    // Cipher.ENCRYPT_MODE / DECRYPT_MODE
    const ENCRYPT_MODE: i32 = 1;
    const DECRYPT_MODE: i32 = 2;

    /// Captured when Android loads the native library — gives us a JVM handle to
    /// attach any thread and reach the Android Keystore.
    #[no_mangle]
    pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut std::ffi::c_void) -> jni::sys::jint {
        let _ = VM.set(vm);
        jni::sys::JNI_VERSION_1_6
    }

    fn e(ctx: &str, err: jni::errors::Error) -> Error {
        Error::Keystore(format!("android keystore {ctx}: {err}"))
    }

    fn to_bytes(env: &mut JNIEnv, obj: JObject) -> Result<Vec<u8>> {
        let arr: JByteArray = obj.into();
        env.convert_byte_array(arr).map_err(|x| e("read byte[]", x))
    }

    /// Get-or-create the AES-256-GCM master key in the Android Keystore.
    fn master_key<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>> {
        let provider = env.new_string("AndroidKeyStore").map_err(|x| e("provider str", x))?;
        let ks = env
            .call_static_method(
                "java/security/KeyStore",
                "getInstance",
                "(Ljava/lang/String;)Ljava/security/KeyStore;",
                &[JValue::Object(&provider)],
            )
            .and_then(|v| v.l())
            .map_err(|x| e("KeyStore.getInstance", x))?;
        env.call_method(
            &ks,
            "load",
            "(Ljava/security/KeyStore$LoadStoreParameter;)V",
            &[JValue::Object(&JObject::null())],
        )
        .map_err(|x| e("KeyStore.load", x))?;

        let alias = env.new_string(ALIAS).map_err(|x| e("alias str", x))?;
        let exists = env
            .call_method(&ks, "containsAlias", "(Ljava/lang/String;)Z", &[JValue::Object(&alias)])
            .and_then(|v| v.z())
            .map_err(|x| e("containsAlias", x))?;
        if exists {
            return env
                .call_method(
                    &ks,
                    "getKey",
                    "(Ljava/lang/String;[C)Ljava/security/Key;",
                    &[JValue::Object(&alias), JValue::Object(&JObject::null())],
                )
                .and_then(|v| v.l())
                .map_err(|x| e("getKey", x));
        }

        // new KeyGenParameterSpec.Builder(alias, ENCRYPT|DECRYPT)
        let builder = env
            .new_object(
                "android/security/keystore/KeyGenParameterSpec$Builder",
                "(Ljava/lang/String;I)V",
                &[JValue::Object(&alias), JValue::Int(PURPOSE_ENCRYPT_DECRYPT)],
            )
            .map_err(|x| e("new Builder", x))?;
        let ret_builder = "Landroid/security/keystore/KeyGenParameterSpec$Builder;";
        // .setBlockModes("GCM")
        let gcm = env.new_string("GCM").map_err(|x| e("GCM str", x))?;
        let block_modes = env.new_object_array(1, "java/lang/String", &gcm).map_err(|x| e("modes[]", x))?;
        let builder = env
            .call_method(&builder, "setBlockModes", &format!("([Ljava/lang/String;){ret_builder}"), &[JValue::Object(&block_modes)])
            .and_then(|v| v.l())
            .map_err(|x| e("setBlockModes", x))?;
        // .setEncryptionPaddings("NoPadding")
        let nopad = env.new_string("NoPadding").map_err(|x| e("NoPadding str", x))?;
        let paddings = env.new_object_array(1, "java/lang/String", &nopad).map_err(|x| e("pads[]", x))?;
        let builder = env
            .call_method(&builder, "setEncryptionPaddings", &format!("([Ljava/lang/String;){ret_builder}"), &[JValue::Object(&paddings)])
            .and_then(|v| v.l())
            .map_err(|x| e("setEncryptionPaddings", x))?;
        // .setKeySize(256)
        let builder = env
            .call_method(&builder, "setKeySize", &format!("(I){ret_builder}"), &[JValue::Int(256)])
            .and_then(|v| v.l())
            .map_err(|x| e("setKeySize", x))?;
        // spec = builder.build()
        let spec = env
            .call_method(&builder, "build", "()Landroid/security/keystore/KeyGenParameterSpec;", &[])
            .and_then(|v| v.l())
            .map_err(|x| e("build", x))?;
        // KeyGenerator kg = KeyGenerator.getInstance("AES", "AndroidKeyStore")
        let aes = env.new_string("AES").map_err(|x| e("AES str", x))?;
        let provider2 = env.new_string("AndroidKeyStore").map_err(|x| e("provider2 str", x))?;
        let kg = env
            .call_static_method(
                "javax/crypto/KeyGenerator",
                "getInstance",
                "(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;",
                &[JValue::Object(&aes), JValue::Object(&provider2)],
            )
            .and_then(|v| v.l())
            .map_err(|x| e("KeyGenerator.getInstance", x))?;
        env.call_method(&kg, "init", "(Ljava/security/spec/AlgorithmParameterSpec;)V", &[JValue::Object(&spec)])
            .map_err(|x| e("KeyGenerator.init", x))?;
        env.call_method(&kg, "generateKey", "()Ljavax/crypto/SecretKey;", &[])
            .and_then(|v| v.l())
            .map_err(|x| e("generateKey", x))
    }

    fn cipher<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>> {
        let t = env.new_string("AES/GCM/NoPadding").map_err(|x| e("transform str", x))?;
        env.call_static_method(
            "javax/crypto/Cipher",
            "getInstance",
            "(Ljava/lang/String;)Ljavax/crypto/Cipher;",
            &[JValue::Object(&t)],
        )
        .and_then(|v| v.l())
        .map_err(|x| e("Cipher.getInstance", x))
    }

    fn vm() -> Result<&'static JavaVM> {
        VM.get().ok_or_else(|| Error::Keystore("JNI_OnLoad not called (no JavaVM)".into()))
    }

    /// Encrypt: returns iv(12) || gcm-ciphertext(+16 tag).
    pub fn seal(plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut env = vm()?.attach_current_thread().map_err(|x| e("attach", x))?;
        let key = master_key(&mut env)?;
        let c = cipher(&mut env)?;
        env.call_method(&c, "init", "(ILjava/security/Key;)V", &[JValue::Int(ENCRYPT_MODE), JValue::Object(&key)])
            .map_err(|x| e("Cipher.init encrypt", x))?;
        let iv_obj = env.call_method(&c, "getIV", "()[B", &[]).and_then(|v| v.l()).map_err(|x| e("getIV", x))?;
        let iv = to_bytes(&mut env, iv_obj)?;
        let input = env.byte_array_from_slice(plaintext).map_err(|x| e("input[]", x))?;
        let ct_obj = env
            .call_method(&c, "doFinal", "([B)[B", &[JValue::Object(&input)])
            .and_then(|v| v.l())
            .map_err(|x| e("doFinal encrypt", x))?;
        let ct = to_bytes(&mut env, ct_obj)?;
        let mut blob = Vec::with_capacity(iv.len() + ct.len());
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&ct);
        Ok(blob)
    }

    /// Decrypt a blob produced by [`seal`].
    pub fn unseal(blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < IV_LEN {
            return Err(Error::Keystore("keystore blob too short".into()));
        }
        let (iv, ct) = blob.split_at(IV_LEN);
        let mut env = vm()?.attach_current_thread().map_err(|x| e("attach", x))?;
        let key = master_key(&mut env)?;
        let c = cipher(&mut env)?;
        let iv_arr = env.byte_array_from_slice(iv).map_err(|x| e("iv[]", x))?;
        let spec = env
            .new_object("javax/crypto/spec/GCMParameterSpec", "(I[B)V", &[JValue::Int(GCM_TAG_BITS), JValue::Object(&iv_arr)])
            .map_err(|x| e("new GCMParameterSpec", x))?;
        env.call_method(
            &c,
            "init",
            "(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V",
            &[JValue::Int(DECRYPT_MODE), JValue::Object(&key), JValue::Object(&spec)],
        )
        .map_err(|x| e("Cipher.init decrypt", x))?;
        let ct_arr = env.byte_array_from_slice(ct).map_err(|x| e("ct[]", x))?;
        let pt_obj = env
            .call_method(&c, "doFinal", "([B)[B", &[JValue::Object(&ct_arr)])
            .and_then(|v| v.l())
            .map_err(|x| e("doFinal decrypt", x))?;
        to_bytes(&mut env, pt_obj)
    }
}

// ── File-backed keystore: plain JSON file (no keychain, no OS prompts) ──────
//
// Selected for debug builds (no OS prompts during dev/test) AND whenever the
// `os-keystore` feature is off — the latter drops the `keyring` dependency
// entirely so a headless build with no `dbus-1` can link. A release build with
// default features on uses the OS keychain backend below, unchanged.

#[cfg(any(debug_assertions, not(feature = "os-keystore")))]
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
        // Mobile uses the OS secure store for real (iOS Keychain / Android
        // Keystore — issue #185). This file-backed path is a compile-complete
        // fallback; the bridge passes POLLIS_DATA_DIR (app sandbox) when wired.
        #[cfg(any(target_os = "ios", target_os = "android"))]
        let base = {
            if let Ok(dir) = std::env::var("POLLIS_DATA_DIR") {
                PathBuf::from(dir)
            } else {
                std::env::temp_dir().join("pollis")
            }
        };
        base.join("dev-keystore.json")
    }

    // On Android the file holds AES-GCM ciphertext (see `super::android_kek`);
    // everywhere else it's plaintext JSON. These convert between the on-disk
    // bytes and the JSON string.
    #[cfg(target_os = "android")]
    fn decode_file(raw: &[u8]) -> Result<String> {
        let plain = super::android_kek::unseal(raw)?;
        String::from_utf8(plain).map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))
    }
    #[cfg(not(target_os = "android"))]
    fn decode_file(raw: &[u8]) -> Result<String> {
        String::from_utf8(raw.to_vec()).map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))
    }
    #[cfg(target_os = "android")]
    fn encode_file(json: &str) -> Result<Vec<u8>> {
        super::android_kek::seal(json.as_bytes())
    }
    #[cfg(not(target_os = "android"))]
    fn encode_file(json: &str) -> Result<Vec<u8>> {
        Ok(json.as_bytes().to_vec())
    }

    fn read_map() -> Result<HashMap<String, String>> {
        let path = store_path();
        let raw = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(e) => return Err(Error::Keystore(format!("read dev-keystore.json: {e}"))),
        };
        let data = decode_file(&raw)?;
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
        let json = serde_json::to_string(map)
            .map_err(|e| Error::Keystore(format!("serialize: {e}")))?;
        // Plaintext bytes on desktop; AES-GCM ciphertext on Android.
        let data = encode_file(&json)?;

        // Atomic write: tempfile + fsync + rename. A crash before the rename
        // leaves the old file intact. Without this, a crash mid-write turned
        // the keystore into a zero-byte file and bounced every user to OTP.
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| Error::Keystore(format!("open dev-keystore.json.tmp: {e}")))?;
            f.write_all(&data)
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
//
// Requires BOTH a release build AND the `os-keystore` feature (on by default),
// so the default desktop release path is unchanged. `--no-default-features`
// drops `keyring` and falls back to the file-backed backend above.

#[cfg(all(not(debug_assertions), feature = "os-keystore"))]
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
