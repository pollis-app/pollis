use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Namespace keyring entries when an explicit data dir is chosen, so instances don't stomp each other. Shipped installs are unprefixed.
fn namespaced(key: &str) -> String {
    format!("{}{key}", namespace_prefix())
}

/// The prefix [`namespaced`] puts in front of every key this process touches.
///
/// Split out for the backend probe (#950): `""` for release, `"DEV:"` for debug, plus `"{label}:"` with an explicit data dir.
fn namespace_prefix() -> String {
    #[cfg(debug_assertions)]
    let base = "DEV:".to_string();
    #[cfg(not(debug_assertions))]
    let base = String::new();

    match crate::db::local::explicit_data_dir() {
        Some(dir) => {
            let label = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("dev2");
            format!("{label}:{base}")
        }
        None => base,
    }
}

/// True if `stored_key` is one of THIS process's keys rather than a sibling
/// instance's.
///
/// Prefix alone is not enough: the unprefixed namespace would match `dev2:db_key_u1`, so the rest must carry no `:`.
#[cfg(any(feature = "os-keystore", test))]
fn is_current_namespace(stored_key: &str) -> bool {
    key_is_in_namespace(&namespace_prefix(), stored_key)
}

/// The pure rule behind [`is_current_namespace`], testable without the process data dir.
#[cfg(any(feature = "os-keystore", test))]
fn key_is_in_namespace(prefix: &str, stored_key: &str) -> bool {
    match stored_key.strip_prefix(prefix) {
        Some(rest) => !rest.contains(':'),
        None => false,
    }
}

// Android: seal the keystore file with AES-256-GCM under a non-exportable Android Keystore key (#185 A). JVM system classes only.
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
    const PURPOSE_ENCRYPT_DECRYPT: i32 = 3;
    const ENCRYPT_MODE: i32 = 1;
    const DECRYPT_MODE: i32 = 2;

    /// Captured at library load: a JVM handle for any thread.
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

        let builder = env
            .new_object(
                "android/security/keystore/KeyGenParameterSpec$Builder",
                "(Ljava/lang/String;I)V",
                &[JValue::Object(&alias), JValue::Int(PURPOSE_ENCRYPT_DECRYPT)],
            )
            .map_err(|x| e("new Builder", x))?;
        let ret_builder = "Landroid/security/keystore/KeyGenParameterSpec$Builder;";
        let gcm = env.new_string("GCM").map_err(|x| e("GCM str", x))?;
        let block_modes = env.new_object_array(1, "java/lang/String", &gcm).map_err(|x| e("modes[]", x))?;
        let builder = env
            .call_method(&builder, "setBlockModes", &format!("([Ljava/lang/String;){ret_builder}"), &[JValue::Object(&block_modes)])
            .and_then(|v| v.l())
            .map_err(|x| e("setBlockModes", x))?;
        let nopad = env.new_string("NoPadding").map_err(|x| e("NoPadding str", x))?;
        let paddings = env.new_object_array(1, "java/lang/String", &nopad).map_err(|x| e("pads[]", x))?;
        let builder = env
            .call_method(&builder, "setEncryptionPaddings", &format!("([Ljava/lang/String;){ret_builder}"), &[JValue::Object(&paddings)])
            .and_then(|v| v.l())
            .map_err(|x| e("setEncryptionPaddings", x))?;
        let builder = env
            .call_method(&builder, "setKeySize", &format!("(I){ret_builder}"), &[JValue::Int(256)])
            .and_then(|v| v.l())
            .map_err(|x| e("setKeySize", x))?;
        let spec = env
            .call_method(&builder, "build", "()Landroid/security/keystore/KeyGenParameterSpec;", &[])
            .and_then(|v| v.l())
            .map_err(|x| e("build", x))?;
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

// iOS: seal the keystore file under a Keychain-held key (`ThisDeviceOnly`, out of backups; #185 A); defends off-device theft, not app-privileged attackers.
#[cfg(target_os = "ios")]
mod ios_kek {
    use super::kek_envelope;
    use crate::error::{Error, Result};
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::passwords::{get_generic_password, set_generic_password_options};
    use security_framework::passwords_options::PasswordOptions;
    use std::sync::Mutex;

    const SERVICE: &str = "pollis";
    const ACCOUNT: &str = "pollis_keystore_kek";
    /// `errSecItemNotFound` — Security.framework's "no such keychain item".
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    /// Cache, and serializes get-or-create: a duplicate `SecItemAdd` silently updates and would orphan the first ciphertext.
    static MASTER: Mutex<Option<[u8; 32]>> = Mutex::new(None);

    fn keychain_err(ctx: &str, err: security_framework::base::Error) -> Error {
        Error::Keystore(format!("ios keychain {ctx}: {err}"))
    }

    /// The keychain access group shared with the Notification Service Extension (#1599), set by
    /// the host. `None` falls back to the `PollisKeychainAccessGroup` Info.plist key.
    static ACCESS_GROUP: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

    /// Cleared by the headless fetch: an extension must never mint a KEK, or the app would
    /// later find the extension's fresh key in the shared group and every sealed byte would be
    /// orphaned.
    static CREATE_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    /// Info.plist key naming the shared access group, e.g. `$(AppIdentifierPrefix)com.pollis.shared`.
    const ACCESS_GROUP_PLIST_KEY: &str = "PollisKeychainAccessGroup";

    pub fn set_access_group(group: &str) {
        if let Ok(mut g) = ACCESS_GROUP.write() {
            *g = Some(group.to_string()).filter(|s| !s.is_empty());
        }
    }

    pub fn forbid_key_creation() {
        CREATE_ALLOWED.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn access_group() -> Option<String> {
        if let Some(g) = ACCESS_GROUP.read().ok().and_then(|g| g.clone()) {
            return Some(g);
        }
        use core_foundation::string::CFString;
        let info = core_foundation::bundle::CFBundle::main_bundle().info_dictionary();
        let value = info.find(CFString::new(ACCESS_GROUP_PLIST_KEY))?;
        let s = value.downcast::<CFString>()?.to_string();
        // An unexpanded build variable means the plist was not processed: treat as unset.
        if s.is_empty() || s.contains("$(") {
            return None;
        }
        Some(s)
    }

    fn protection() -> Result<SecAccessControl> {
        SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
            0,
        )
        .map_err(|x| keychain_err("access control", x))
    }

    fn to_key(bytes: Vec<u8>) -> Result<[u8; 32]> {
        bytes.try_into().map_err(|_| {
            // Refuse: regenerating would orphan every sealed byte.
            Error::Keystore("ios keychain kek has unexpected size; refusing to overwrite".into())
        })
    }

    fn store_new(key: &[u8; 32], group: Option<&str>) -> Result<()> {
        let mut opts = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
        opts.set_access_control(protection()?);
        if let Some(g) = group {
            opts.set_access_group(g);
        }
        set_generic_password_options(key, opts).map_err(|x| keychain_err("store kek", x))
    }

    fn read_in(group: &str) -> std::result::Result<Vec<u8>, security_framework::base::Error> {
        let mut opts = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
        opts.set_access_group(group);
        security_framework::passwords::generic_password(opts)
    }

    /// Copy the KEK from the app's own access group into the shared one (#1599): write, then
    /// read back. The app-only item is KEPT: a later build without the entitlement (or a
    /// mis-signed one) still finds it, so the identity key and the wrapped DB key it seals can
    /// never be orphaned by a migration.
    fn copy_into(group: &str, key: &[u8; 32]) {
        if let Err(e) = store_new(key, Some(group)) {
            crate::diag!("[keystore] ios kek not copied to the shared access group ({e}); the extension cannot read it yet");
            return;
        }
        match read_in(group) {
            Ok(back) if back.as_slice() == key.as_slice() => {}
            _ => crate::diag!("[keystore] ios kek copy in the shared group did not read back"),
        }
    }

    fn shared_read(group: Option<&str>) -> super::kek_plan::Read {
        use super::kek_plan::Read;
        let Some(g) = group else {
            return Read::NoGroup;
        };
        match read_in(g) {
            Ok(bytes) => Read::Found(bytes),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Read::NotFound,
            Err(e) => {
                // e.g. errSecMissingEntitlement (-34018) on a build signed without the group.
                crate::diag!("[keystore] shared access group unreadable ({e}); using the app-only item");
                Read::Error
            }
        }
    }

    fn legacy_read() -> super::kek_plan::Read {
        use super::kek_plan::Read;
        match get_generic_password(SERVICE, ACCOUNT) {
            Ok(bytes) => Read::Found(bytes),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Read::NotFound,
            Err(e) => {
                crate::diag!("[keystore] ios kek unreadable ({e})");
                Read::Error
            }
        }
    }

    /// Get-or-create the 32-byte master key in the iOS Keychain. The decision is
    /// [`super::kek_plan::plan`], host-tested.
    fn master_key() -> Result<[u8; 32]> {
        use super::kek_plan::{plan, Plan, Read};
        let mut guard = MASTER
            .lock()
            .map_err(|_| Error::Keystore("ios kek mutex poisoned".into()))?;
        if let Some(k) = *guard {
            return Ok(k);
        }
        let group = access_group();
        let shared = shared_read(group.as_deref());
        // Only consulted when the shared group did not answer.
        let legacy = if matches!(shared, Read::Found(_)) {
            Read::NotFound
        } else {
            legacy_read()
        };
        let decision = plan(
            shared,
            legacy,
            CREATE_ALLOWED.load(std::sync::atomic::Ordering::SeqCst),
            super::file_backend::store_exists(),
        );
        let key = match decision {
            Plan::Use { key, copy_to_shared } => {
                let key = to_key(key)?;
                if copy_to_shared {
                    if let Some(ref g) = group {
                        copy_into(g, &key);
                    }
                }
                key
            }
            Plan::Mint { into_shared } => {
                let mut fresh = [0u8; 32];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut fresh);
                // Always app-only, so a build without the entitlement finds it too; and the
                // shared group when it answered, so the extension can.
                store_new(&fresh, None)?;
                if into_shared {
                    if let Some(ref g) = group {
                        copy_into(g, &fresh);
                    }
                }
                fresh
            }
            Plan::Refuse(why) => return Err(Error::Keystore(why.into())),
        };
        *guard = Some(key);
        Ok(key)
    }

    /// Encrypt: returns iv(12) || gcm-ciphertext(+16 tag).
    pub fn seal(plaintext: &[u8]) -> Result<Vec<u8>> {
        kek_envelope::seal_with(&master_key()?, plaintext)
    }

    /// Decrypt a blob produced by [`seal`].
    pub fn unseal(blob: &[u8]) -> Result<Vec<u8>> {
        kek_envelope::unseal_with(&master_key()?, blob)
    }
}

/// Which keystore KEK an iOS process uses, from what the keychain answered (#1599). Pure, so the
/// rules that decide whether keys can be orphaned are tested on the host.
#[cfg(any(target_os = "ios", test))]
mod kek_plan {
    /// One keychain lookup.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Read {
        Found(Vec<u8>),
        NotFound,
        /// Any other failure, e.g. errSecMissingEntitlement on a build without the group.
        Error,
        /// No shared group configured.
        NoGroup,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Plan {
        /// Use this key; `copy_to_shared` = also copy it into the shared group (never a move).
        Use { key: Vec<u8>, copy_to_shared: bool },
        /// No key exists anywhere and nothing is sealed yet: make one.
        Mint { into_shared: bool },
        /// Never mint over existing ciphertext, and never in the extension.
        Refuse(&'static str),
    }

    /// `shared`: the shared access group's item. `legacy`: the app-only lookup (any group the
    /// process can see). `create_allowed`: false in the extension. `store_exists`: a sealed
    /// `keystore.pks` is already on disk.
    ///
    /// There is deliberately no "delete" outcome: the app-only item always survives.
    pub fn plan(shared: Read, legacy: Read, create_allowed: bool, store_exists: bool) -> Plan {
        if let Read::Found(key) = shared {
            return Plan::Use {
                key,
                copy_to_shared: false,
            };
        }
        // Copy only when the group answered "not there": an error means we cannot write it either.
        let group_usable = shared == Read::NotFound;
        match legacy {
            Read::Found(key) => Plan::Use {
                key,
                copy_to_shared: create_allowed && group_usable,
            },
            Read::Error => Plan::Refuse("the keystore key is unreadable"),
            Read::NotFound | Read::NoGroup => {
                if !create_allowed {
                    Plan::Refuse("no keystore key this process may use; the extension never creates one")
                } else if store_exists {
                    Plan::Refuse(
                        "the keystore key is missing but a sealed keystore exists; refusing to mint a new key over it",
                    )
                } else {
                    Plan::Mint {
                        into_shared: group_usable,
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        const K: &[u8] = &[1u8; 32];

        fn used(key: &[u8], copy_to_shared: bool) -> Plan {
            Plan::Use {
                key: key.to_vec(),
                copy_to_shared,
            }
        }

        #[test]
        fn the_shared_item_wins() {
            assert_eq!(plan(Read::Found(K.to_vec()), Read::Found(vec![2; 32]), true, true), used(K, false));
        }

        #[test]
        fn the_app_only_item_is_copied_never_moved() {
            assert_eq!(plan(Read::NotFound, Read::Found(K.to_vec()), true, true), used(K, true));
        }

        #[test]
        fn a_missing_entitlement_falls_back_to_the_app_only_item() {
            // errSecMissingEntitlement reading the shared group: use the old item, copy nothing.
            assert_eq!(plan(Read::Error, Read::Found(K.to_vec()), true, true), used(K, false));
        }

        #[test]
        fn a_sealed_store_is_never_orphaned_by_a_fresh_key() {
            for shared in [Read::NotFound, Read::Error, Read::NoGroup] {
                let p = plan(shared.clone(), Read::NotFound, true, true);
                assert!(matches!(p, Plan::Refuse(_)), "{shared:?}: {p:?}");
            }
        }

        #[test]
        fn the_extension_never_mints_or_copies() {
            for store in [true, false] {
                assert!(matches!(plan(Read::NotFound, Read::NotFound, false, store), Plan::Refuse(_)));
            }
            assert_eq!(plan(Read::NotFound, Read::Found(K.to_vec()), false, true), used(K, false));
        }

        #[test]
        fn a_fresh_install_mints_into_the_group_when_it_answers() {
            assert_eq!(plan(Read::NotFound, Read::NotFound, true, false), Plan::Mint { into_shared: true });
            assert_eq!(plan(Read::NoGroup, Read::NotFound, true, false), Plan::Mint { into_shared: false });
            assert_eq!(plan(Read::Error, Read::NotFound, true, false), Plan::Mint { into_shared: false });
        }

        #[test]
        fn an_unreadable_app_only_item_is_never_replaced() {
            assert!(matches!(plan(Read::NotFound, Read::Error, true, false), Plan::Refuse(_)));
        }
    }
}

/// iOS (#1599): the keychain access group the keystore KEK lives in, shared with the
/// Notification Service Extension. Call before anything touches the keystore.
#[cfg(target_os = "ios")]
pub fn set_ios_keychain_access_group(group: &str) {
    ios_kek::set_access_group(group);
}

/// For a headless process (#1599): never mint a keystore KEK. On iOS a missing shared KEK
/// becomes an error instead of a fresh key; elsewhere there is nothing to forbid.
pub fn forbid_key_creation() {
    #[cfg(target_os = "ios")]
    ios_kek::forbid_key_creation();
}

// AES-256-GCM envelope for iOS and the desktop file store (#882); iv(12) || ct(+16), host-tested.
#[cfg(not(target_os = "android"))]
mod kek_envelope {
    use crate::error::{Error, Result};
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    const IV_LEN: usize = 12;

    pub fn seal_with(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let mut iv = [0u8; IV_LEN];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut iv);
        let ct = cipher
            .encrypt(Nonce::from_slice(&iv), plaintext)
            .map_err(|_| Error::Keystore("keystore seal failed".into()))?;
        let mut blob = Vec::with_capacity(IV_LEN + ct.len());
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&ct);
        Ok(blob)
    }

    pub fn unseal_with(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < IV_LEN {
            return Err(Error::Keystore("keystore blob too short".into()));
        }
        let (iv, ct) = blob.split_at(IV_LEN);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        cipher
            .decrypt(Nonce::from_slice(iv), ct)
            .map_err(|_| Error::Keystore("keystore unseal failed (wrong key or tampered blob)".into()))
    }
}


// Desktop (#882): the file KEK is HKDF over the machine ID, so a copied data dir is inert elsewhere; not a defence against same-UID or full-disk attackers.
// The PIN is not mixed in: the file is read before a PIN exists, and its secrets are already PIN-wrapped. No TPM (heavy dep, half-integration risk).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod machine_kek {
    use crate::error::{Error, Result};
    use hkdf::Hkdf;
    use sha2::Sha256;
    use std::sync::OnceLock;
    use zeroize::Zeroizing;

    /// HKDF domain separator; the file's magic byte versions the format.
    const HKDF_INFO: &[u8] = b"pollis-keystore-file-kek-v1";

    /// Per-file salt, fresh on every write, so two installs on one machine never share a KEK.
    pub const SALT_LEN: usize = 16;

    pub const KEK_LEN: usize = 32;

    /// Last-resort override for hosts without a machine ID (see `docs/run-it-yourself.md`); not a knob.
    const MACHINE_ID_ENV: &str = "POLLIS_KEYSTORE_MACHINE_ID";

    static SECRET: OnceLock<Option<Zeroizing<Vec<u8>>>> = OnceLock::new();

    /// The machine-bound secret, read once per process.
    ///
    /// Hard error with no source: a constant or a sibling file would be fake encryption or a new way to lose keys.
    pub fn machine_secret() -> Result<&'static [u8]> {
        SECRET
            .get_or_init(read_machine_secret)
            .as_deref()
            .map(|s| &s[..])
            .ok_or_else(|| {
                Error::Keystore(format!(
                    "no stable machine identity found for at-rest keystore encryption \
                     (tried the platform machine ID); set {MACHINE_ID_ENV} to a \
                     stable, private, host-specific value to proceed"
                ))
            })
    }

    fn read_machine_secret() -> Option<Zeroizing<Vec<u8>>> {
        if let Ok(v) = std::env::var(MACHINE_ID_ENV) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(Zeroizing::new(v.into_bytes()));
            }
        }
        platform_machine_id().map(|v| Zeroizing::new(v.into_bytes()))
    }

    /// systemd's machine ID, else D-Bus's.
    #[cfg(target_os = "linux")]
    fn platform_machine_id() -> Option<String> {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(s) = std::fs::read_to_string(path) {
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }

    /// IOPlatformUUID via `ioreg`; only reached without a Keychain.
    #[cfg(target_os = "macos")]
    fn platform_machine_id() -> Option<String> {
        let out = std::process::Command::new("/usr/sbin/ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("IOPlatformUUID") {
                if let Some(value) = line.split('=').nth(1) {
                    let value = value.trim().trim_matches('"').trim().to_string();
                    if !value.is_empty() {
                        return Some(value);
                    }
                }
            }
        }
        None
    }

    /// MachineGuid; `/reg:64` avoids the WOW6432Node view, `CREATE_NO_WINDOW` avoids a console flash.
    #[cfg(target_os = "windows")]
    fn platform_machine_id() -> Option<String> {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
                "/reg:64",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("MachineGuid") {
                if let Some(value) = line.split_whitespace().last() {
                    let value = value.trim().to_string();
                    if !value.is_empty() {
                        return Some(value);
                    }
                }
            }
        }
        None
    }

    /// Pure derivation, so tests can model "copied to another machine".
    pub fn derive_kek(machine_secret: &[u8], salt: &[u8; SALT_LEN]) -> Zeroizing<[u8; KEK_LEN]> {
        let hk = Hkdf::<Sha256>::new(Some(salt), machine_secret);
        let mut out = Zeroizing::new([0u8; KEK_LEN]);
        hk.expand(HKDF_INFO, &mut *out)
            .expect("32-byte HKDF output is always in range");
        out
    }
}

// File-backed keystore: debug builds, `os-keystore` off, or no usable OS keychain (#882); mobile's only backend. Always ciphertext.

mod file_backend {
    use super::namespaced;
    use crate::error::{Error, Result};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// Renamed in #950: it is the production store on headless installs and is not JSON.
    fn store_path() -> PathBuf {
        // The same directory as the local DB, so the two can't drift.
        crate::db::local::dirs_path().join("keystore.pks")
    }

    /// Whether a keystore file (current or pre-#950) is already on disk (#1599): the iOS KEK
    /// must never be re-minted over one.
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    pub(super) fn store_exists() -> bool {
        let nonempty = |p: PathBuf| std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false);
        nonempty(store_path()) || nonempty(legacy_store_path())
    }

    /// The pre-#950 location; read and migrated, never written.
    fn legacy_store_path() -> PathBuf {
        crate::db::local::dirs_path().join("dev-keystore.json")
    }

    /// Pick this process's file, doing the #950 rename (write, verify, then unlink) if needed:
    ///
    ///   1. **write the new file** (atomically, via `write_map_at` — tempfile,
    ///      fsync, rename);
    ///   2. **read it back** and check it reproduces the same map;
    ///   3. **only then unlink the old one.**
    ///
    /// A crash between 1 and 3 leaves both files, and the retry is idempotent; "old gone, new bad" can't happen.
    ///
    /// Every failure falls back to the legacy path: housekeeping must never lock a user out.
    fn resolve_store(codec: &FileCodec) -> PathBuf {
        resolve_store_at(&store_path(), &legacy_store_path(), codec)
    }

    /// [`resolve_store`] with explicit paths, for tests.
    fn resolve_store_at(new: &Path, old: &Path, codec: &FileCodec) -> PathBuf {
        let new = new.to_path_buf();
        let old = old.to_path_buf();

        if new.exists() {
            // Step 3 retry: `new` is authoritative; remove the stale copy, never failing on it.
            if old.exists() {
                match std::fs::remove_file(&old) {
                    Ok(()) => crate::diag!(
                        "[keystore] removed the superseded {} (#950)",
                        old.display()
                    ),
                    Err(e) => crate::diag!(
                        "[keystore] could not remove the superseded {}: {e}",
                        old.display()
                    ),
                }
            }
            return new;
        }

        if !old.exists() {
            return new;
        }

        // 1. Read the old store; failures surface normally and delete nothing.
        let (map, _) = match read_map_at(&old, codec) {
            Ok(m) => m,
            Err(e) => {
                crate::diag!(
                    "[keystore] cannot read {} ({e}); leaving it exactly where it is",
                    old.display()
                );
                return old;
            }
        };

        if let Err(e) = write_map_at(&new, codec, &map) {
            crate::diag!(
                "[keystore] could not write {} ({e}); staying on {}",
                new.display(),
                old.display()
            );
            return old;
        }
        match read_map_at(&new, codec) {
            Ok((got, false)) if got == map => {}
            other => {
                // Keep the old file. `other` is not printed: it may hold decrypted key material.
                let _ = other;
                crate::diag!(
                    "[keystore] {} did not read back correctly; staying on {}",
                    new.display(),
                    old.display()
                );
                let _ = std::fs::remove_file(&new);
                return old;
            }
        }

        match std::fs::remove_file(&old) {
            Ok(()) => crate::diag!(
                "[keystore] renamed the keystore to {} (#950)",
                new.display()
            ),
            Err(e) => crate::diag!(
                "[keystore] wrote {} but could not remove {}: {e} — the next \
                 start will retry",
                new.display(),
                old.display()
            ),
        }
        new
    }

    /// Desktop at-rest layout, written by [`FileCodec::encode`]:
    ///
    /// ```text
    /// magic  (4)  = b"PKS\x01"   — 'P' also rules out a legacy '{' JSON start
    /// salt   (16) = HKDF salt, fresh every write
    /// body   (..) = kek_envelope blob: iv(12) || AES-256-GCM(json) || tag(16)
    /// ```
    ///
    /// No AAD needed: the salt feeds the KEK, so substituting it fails the tag.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    const FILE_MAGIC: [u8; 4] = *b"PKS\x01";

    /// `LegacyPlaintext` is the pre-#882 desktop file, readable so upgraders aren't locked out.
    enum Decoded {
        Encrypted(String),
        #[cfg_attr(
            any(target_os = "android", target_os = "ios"),
            allow(dead_code)
        )]
        LegacyPlaintext(String),
    }

    /// The at-rest codec: a machine-bound KEK on desktop, the OS key on mobile.
    struct FileCodec {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        machine_secret: Vec<u8>,
        /// Test-only: a host with no derivable KEK.
        #[cfg(test)]
        fail_seal: bool,
    }

    impl FileCodec {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        fn detect() -> Result<Self> {
            Ok(Self {
                machine_secret: super::machine_kek::machine_secret()?.to_vec(),
                #[cfg(test)]
                fail_seal: false,
            })
        }

        #[cfg(any(target_os = "android", target_os = "ios"))]
        fn detect() -> Result<Self> {
            Ok(Self {
                #[cfg(test)]
                fail_seal: false,
            })
        }

        #[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
        fn with_secret(machine_secret: &[u8]) -> Self {
            Self {
                machine_secret: machine_secret.to_vec(),
                fail_seal: false,
            }
        }

        #[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
        fn that_cannot_seal() -> Self {
            Self {
                machine_secret: b"unused".to_vec(),
                fail_seal: true,
            }
        }

        /// Can decode `machine_secret`'s files but not encode, so a #950 test reaches the write.
        #[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
        fn with_secret_but_cannot_seal(machine_secret: &[u8]) -> Self {
            Self {
                machine_secret: machine_secret.to_vec(),
                fail_seal: true,
            }
        }

        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        fn encode(&self, json: &str) -> Result<Vec<u8>> {
            use super::machine_kek::SALT_LEN;

            #[cfg(test)]
            if self.fail_seal {
                return Err(Error::Keystore("no machine identity (test)".into()));
            }

            let mut salt = [0u8; SALT_LEN];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
            let kek = super::machine_kek::derive_kek(&self.machine_secret, &salt);
            let body = super::kek_envelope::seal_with(&kek, json.as_bytes())?;

            let mut out = Vec::with_capacity(FILE_MAGIC.len() + SALT_LEN + body.len());
            out.extend_from_slice(&FILE_MAGIC);
            out.extend_from_slice(&salt);
            out.extend_from_slice(&body);
            Ok(out)
        }

        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        fn decode(&self, raw: &[u8]) -> Result<Decoded> {
            use super::machine_kek::SALT_LEN;

            const HEADER: usize = 4 + SALT_LEN;
            if raw.len() >= FILE_MAGIC.len() && raw[..FILE_MAGIC.len()] == FILE_MAGIC {
                if raw.len() < HEADER {
                    return Err(Error::Keystore(
                        "keystore file header truncated; refusing to treat it as empty".into(),
                    ));
                }
                let mut salt = [0u8; SALT_LEN];
                salt.copy_from_slice(&raw[4..HEADER]);
                let kek = super::machine_kek::derive_kek(&self.machine_secret, &salt);
                // "Wrong machine", not corrupt: surface it and leave the file alone.
                let plain = super::kek_envelope::unseal_with(&kek, &raw[HEADER..]).map_err(|_| {
                    Error::Keystore(
                        "keystore file could not be decrypted on this machine — it is \
                         encrypted under a machine-bound key and was written by a \
                         different host (or the host identity changed). The file has \
                         been left untouched."
                            .into(),
                    )
                })?;
                let json = String::from_utf8(plain)
                    .map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))?;
                return Ok(Decoded::Encrypted(json));
            }

            // Pre-#882 plaintext JSON; the next write re-encrypts it.
            let json = String::from_utf8(raw.to_vec())
                .map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))?;
            Ok(Decoded::LegacyPlaintext(json))
        }

        #[cfg(target_os = "android")]
        fn encode(&self, json: &str) -> Result<Vec<u8>> {
            super::android_kek::seal(json.as_bytes())
        }

        #[cfg(target_os = "android")]
        fn decode(&self, raw: &[u8]) -> Result<Decoded> {
            let plain = super::android_kek::unseal(raw)?;
            let json = String::from_utf8(plain)
                .map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))?;
            Ok(Decoded::Encrypted(json))
        }

        #[cfg(target_os = "ios")]
        fn encode(&self, json: &str) -> Result<Vec<u8>> {
            super::ios_kek::seal(json.as_bytes())
        }

        #[cfg(target_os = "ios")]
        fn decode(&self, raw: &[u8]) -> Result<Decoded> {
            let plain = super::ios_kek::unseal(raw)?;
            let json = String::from_utf8(plain)
                .map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))?;
            Ok(Decoded::Encrypted(json))
        }
    }

    /// The bool is "still pre-#882 plaintext", triggering re-encryption.
    fn read_map_at(
        path: &Path,
        codec: &FileCodec,
    ) -> Result<(HashMap<String, String>, bool)> {
        let raw = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((HashMap::new(), false))
            }
            Err(e) => {
                return Err(Error::Keystore(format!(
                    "read {}: {e}",
                    path.display()
                )))
            }
        };
        // Only a *parse* failure after a successful decrypt is corruption.
        let (data, legacy) = match codec.decode(&raw)? {
            Decoded::Encrypted(json) => (json, false),
            Decoded::LegacyPlaintext(json) => (json, true),
        };
        match serde_json::from_str(&data) {
            Ok(m) => Ok((m, legacy)),
            Err(parse_err) => {
                // #950: the file is LEFT WHERE IT IS; renaming it away silently signed users out and orphaned their identity key.
                //
                // A loud, stable, recoverable failure instead; wiping is the user's choice (`commands::auth::wipe_local_data`).
                crate::diag!(
                    "[keystore] {} decrypted but did not parse ({parse_err}); \
                     leaving it untouched — it may still hold recoverable keys",
                    path.display()
                );
                Err(Error::Keystore(format!(
                    "the keystore at {} is unreadable ({parse_err}). It has been \
                     left untouched in case its contents can be recovered; \
                     nothing has been deleted. To start over on this device and \
                     re-authenticate, use \"wipe this computer\" on the login \
                     screen.",
                    path.display()
                )))
            }
        }
    }

    /// The staging file: the store's own name plus `.tmp`.
    fn tmp_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("keystore.pks");
        path.with_file_name(format!("{name}.tmp"))
    }

    fn write_map_at(
        path: &Path,
        codec: &FileCodec,
        map: &HashMap<String, String>,
    ) -> Result<()> {
        use std::io::Write;

        if let Some(parent) = path.parent() {
            crate::private_fs::create_dir_all(parent)
                .map_err(|e| Error::Keystore(format!("create dir: {e}")))?;
        }
        let json = serde_json::to_string(map)
            .map_err(|e| Error::Keystore(format!("serialize: {e}")))?;
        // Ciphertext always, encoded BEFORE the old file is touched.
        let data = codec.encode(&json)?;

        // Atomic tempfile + fsync + rename, so a crash leaves the complete old or new file, never a mix.
        let tmp = tmp_path(path);
        {
            // Owner-only; `rename` carries the mode.
            let mut f = crate::private_fs::create_file(&tmp)
                .map_err(|e| Error::Keystore(format!("open {}: {e}", tmp.display())))?;
            f.write_all(&data)
                .map_err(|e| Error::Keystore(format!("write {}: {e}", tmp.display())))?;
            f.sync_all()
                .map_err(|e| Error::Keystore(format!("fsync {}: {e}", tmp.display())))?;
        }
        std::fs::rename(&tmp, path)
            .map_err(|e| Error::Keystore(format!("rename {}: {e}", tmp.display())))?;
        Ok(())
    }

    // Explicit path + codec so tests drive the runtime code, migration included.

    fn store_at(
        path: &Path,
        codec: &FileCodec,
        key: String,
        encoded: String,
    ) -> Result<()> {
        let (mut map, _) = read_map_at(path, codec)?;
        map.insert(key, encoded);
        write_map_at(path, codec, &map)
    }

    fn load_at(path: &Path, codec: &FileCodec, key: &str) -> Result<Option<Vec<u8>>> {
        let (map, legacy) = read_map_at(path, codec)?;

        // Best-effort migration off plaintext; it must never fail the read.
        if legacy {
            match write_map_at(path, codec, &map) {
                Ok(()) => crate::diag!(
                    "[keystore] re-wrote the plaintext keystore encrypted at rest (#882)"
                ),
                Err(e) => crate::diag!(
                    "[keystore] could not re-encrypt the plaintext keystore ({e}); \
                     it is unchanged and still readable"
                ),
            }
        }

        match map.get(key) {
            None => Ok(None),
            Some(encoded) => {
                let bytes =
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
                        .map_err(|e| Error::Keystore(format!("base64 decode: {e}")))?;
                Ok(Some(bytes))
            }
        }
    }

    fn delete_at(path: &Path, codec: &FileCodec, key: &str) -> Result<()> {
        let (mut map, _) = read_map_at(path, codec)?;
        map.remove(key);
        write_map_at(path, codec, &map)
    }

    pub async fn store(key: &str, value: &[u8]) -> Result<()> {
        let key = namespaced(key);
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value);
        tokio::task::spawn_blocking(move || {
            // No KEK ⇒ a loud error, never a plaintext fallback.
            let codec = FileCodec::detect()?;
            store_at(&resolve_store(&codec), &codec, key, encoded)
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn load(key: &str) -> Result<Option<Vec<u8>>> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let codec = FileCodec::detect()?;
            load_at(&resolve_store(&codec), &codec, &key)
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    pub async fn delete(key: &str) -> Result<()> {
        let key = namespaced(key);
        tokio::task::spawn_blocking(move || {
            let codec = FileCodec::detect()?;
            delete_at(&resolve_store(&codec), &codec, &key)
        })
        .await
        .map_err(|e| Error::Keystore(format!("spawn_blocking: {e}")))?
    }

    /// Predicate half of [`holds_current_namespace_keys`].
    #[cfg(any(feature = "os-keystore", test))]
    fn map_holds_current_namespace_keys(map: &HashMap<String, String>) -> bool {
        map.keys().any(|k| super::is_current_namespace(k))
    }

    /// Does the file store already hold keys belonging to THIS namespace?
    ///
    /// Asked by [`super::backend`] before switching to a newly available keychain (#950).
    ///
    /// **An unreadable store answers `false`**: staying on it buys nothing.
    #[cfg(feature = "os-keystore")]
    pub fn holds_current_namespace_keys() -> bool {
        let Ok(codec) = FileCodec::detect() else {
            return false;
        };
        match read_map_at(&resolve_store(&codec), &codec) {
            Ok((map, _)) => map_holds_current_namespace_keys(&map),
            Err(e) => {
                crate::diag!(
                    "[keystore] could not inspect the file store while choosing a \
                     backend ({e}); treating it as empty"
                );
                false
            }
        }
    }

    // Tests use an EXPLICIT machine secret, so "copied to another machine" is first-class.
    #[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
    mod tests {
        use super::*;

        const MACHINE_A: &[u8] = b"machine-a-11111111111111111111111111";
        const MACHINE_B: &[u8] = b"machine-b-22222222222222222222222222";

        fn map_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        }

        #[test]
        fn file_roundtrips_through_the_encrypted_format() {
            let dir = std::env::temp_dir().join(format!(
                "pollis-ks-rt-{}",
                ulid::Ulid::new()
            ));
            let path = dir.join("keystore.pks");
            let codec = FileCodec::with_secret(MACHINE_A);

            let want = map_of(&[("account_id_key_wrapped_u1", "c2VjcmV0")]);
            write_map_at(&path, &codec, &want).unwrap();
            let (got, legacy) = read_map_at(&path, &codec).unwrap();

            assert_eq!(got, want);
            assert!(!legacy, "a file we just wrote is not legacy plaintext");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// GUARD (#882): fails if a plaintext encode path returns.
        #[test]
        fn guard_written_bytes_are_never_plaintext() {
            let dir = std::env::temp_dir().join(format!(
                "pollis-ks-guard-{}",
                ulid::Ulid::new()
            ));
            let path = dir.join("keystore.pks");
            let codec = FileCodec::with_secret(MACHINE_A);

            let secret_slot = "account_id_key_wrapped_u1";
            let secret_value = "VEhJUy1JUy1BLUtFWQ";
            write_map_at(&path, &codec, &map_of(&[(secret_slot, secret_value)])).unwrap();

            let raw = std::fs::read(&path).unwrap();
            assert_eq!(&raw[..4], &FILE_MAGIC, "file must carry the sealed-format magic");
            let text = String::from_utf8_lossy(&raw);
            assert!(
                !text.contains(secret_slot),
                "slot names must not be readable on disk"
            );
            assert!(
                !text.contains(secret_value),
                "stored values must not be readable on disk"
            );
            // Random ciphertext may contain `{`; this only pins that it isn't a JSON document.
            assert_ne!(raw[0], b'{', "the file must not start a JSON object");
            assert!(
                serde_json::from_slice::<HashMap<String, String>>(&raw).is_err(),
                "the file must not parse as the plaintext store"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Machine binding: a different host secret refuses.
        #[test]
        fn file_copied_to_another_machine_does_not_decrypt() {
            let codec_a = FileCodec::with_secret(MACHINE_A);
            let codec_b = FileCodec::with_secret(MACHINE_B);

            let raw = codec_a.encode(r#"{"k":"v"}"#).unwrap();
            assert!(matches!(
                codec_a.decode(&raw).unwrap(),
                Decoded::Encrypted(_)
            ));

            // Matched, not `unwrap_err`d: `Decoded` must never gain `Debug`.
            let err = match codec_b.decode(&raw) {
                Ok(_) => panic!("a foreign machine must not be able to decrypt the store"),
                Err(e) => e,
            };
            assert!(
                err.to_string().contains("different host"),
                "wrong-machine error should name the cause, got: {err}"
            );
        }

        /// AES-GCM must REJECT a tampered file, not hand back garbage.
        #[test]
        fn tampered_ciphertext_is_rejected_not_silently_accepted() {
            let codec = FileCodec::with_secret(MACHINE_A);
            let raw = codec.encode(r#"{"k":"v"}"#).unwrap();

            let mut body_flip = raw.clone();
            let last = body_flip.len() - 1;
            body_flip[last] ^= 1;
            assert!(codec.decode(&body_flip).is_err(), "GCM tag must catch a body flip");

            let mut salt_flip = raw.clone();
            salt_flip[5] ^= 1;
            assert!(codec.decode(&salt_flip).is_err(), "header must be bound to the body");

            assert!(codec.decode(&raw[..20]).is_err(), "truncation must not panic");

            assert!(codec.decode(&FILE_MAGIC).is_err(), "short header must error");
        }

        /// An undecryptable file is left exactly as it was.
        #[test]
        fn undecryptable_file_is_left_on_disk_untouched() {
            let dir = std::env::temp_dir().join(format!(
                "pollis-ks-untouched-{}",
                ulid::Ulid::new()
            ));
            let path = dir.join("keystore.pks");

            let codec_a = FileCodec::with_secret(MACHINE_A);
            write_map_at(&path, &codec_a, &map_of(&[("k", "v")])).unwrap();
            let before = std::fs::read(&path).unwrap();

            let codec_b = FileCodec::with_secret(MACHINE_B);
            assert!(read_map_at(&path, &codec_b).is_err());

            let after = std::fs::read(&path).unwrap();
            assert_eq!(before, after, "an undecryptable store must not be rewritten");
            assert!(
                !tmp_path(&path).exists(),
                "no partial file should be left behind"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Pre-#882 plaintext reads, and the next write re-encrypts the same keys.
        #[test]
        fn plaintext_file_migrates_to_encrypted_without_losing_keys() {
            let dir = std::env::temp_dir().join(format!(
                "pollis-ks-migrate-{}",
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("keystore.pks");

            let legacy = map_of(&[
                ("device_id_u1", "ZGV2aWNl"),
                ("account_id_key_wrapped_u1", "a2V5"),
            ]);
            std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();

            let codec = FileCodec::with_secret(MACHINE_A);
            let (got, is_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(got, legacy, "plaintext must still be readable");
            assert!(is_legacy, "a plaintext file must be reported as legacy");

            write_map_at(&path, &codec, &got).unwrap();

            let raw = std::fs::read(&path).unwrap();
            assert_eq!(&raw[..4], &FILE_MAGIC, "migrated file must be sealed");
            let (after, still_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(after, legacy, "migration must preserve every key");
            assert!(!still_legacy, "migration must not need to run twice");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A crash before the rename: the original survives and the retry succeeds.
        #[test]
        fn interrupted_migration_keeps_every_key() {
            let dir = std::env::temp_dir().join(format!(
                "pollis-ks-interrupt-{}",
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("keystore.pks");

            let legacy = map_of(&[("account_id_key_wrapped_u1", "a2V5")]);
            std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();

            let codec = FileCodec::with_secret(MACHINE_A);
            let partial = codec.encode(r#"{"account_id_key_wrapped_u1":"a2V5"}"#).unwrap();
            std::fs::write(tmp_path(&path), &partial[..10]).unwrap();

            let (recovered, is_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(recovered, legacy, "an interrupted migration loses nothing");
            assert!(is_legacy, "the original is still the pre-#882 file");

            write_map_at(&path, &codec, &recovered).unwrap();
            let (after, still_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(after, legacy);
            assert!(!still_legacy);
            std::fs::remove_dir_all(&dir).ok();
        }

        /// No KEK fails the WRITE, never falling back to plaintext; the file survives.
        #[test]
        fn a_write_that_cannot_seal_leaves_the_old_file_intact() {
            let dir = std::env::temp_dir().join(format!(
                "pollis-ks-noseal-{}",
                ulid::Ulid::new()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("keystore.pks");

            let legacy = map_of(&[("account_id_key_wrapped_u1", "a2V5")]);
            let bytes = serde_json::to_string(&legacy).unwrap();
            std::fs::write(&path, &bytes).unwrap();

            let codec = FileCodec::that_cannot_seal();
            let err = write_map_at(&path, &codec, &legacy).unwrap_err();
            assert!(err.to_string().contains("no machine identity"));

            // Untouched: encoding precedes the temp file.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
            assert!(!tmp_path(&path).exists());
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Fresh salt AND nonce every write.
        #[test]
        fn every_write_uses_fresh_salt_and_nonce() {
            let codec = FileCodec::with_secret(MACHINE_A);
            let a = codec.encode(r#"{"k":"v"}"#).unwrap();
            let b = codec.encode(r#"{"k":"v"}"#).unwrap();
            assert_ne!(a[4..20], b[4..20], "salt must be fresh per write");
            assert_ne!(a[20..32], b[20..32], "nonce must be fresh per write");
        }

        /// The real store/load/delete triple, end to end.
        #[test]
        fn store_load_delete_roundtrip_through_the_real_operations() {
            let dir = std::env::temp_dir().join(format!("pollis-ks-ops-{}", ulid::Ulid::new()));
            let path = dir.join("keystore.pks");
            let codec = FileCodec::with_secret(MACHINE_A);

            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"identity");
            store_at(&path, &codec, "account_id_key_wrapped_u1".into(), encoded).unwrap();

            assert_eq!(
                load_at(&path, &codec, "account_id_key_wrapped_u1").unwrap(),
                Some(b"identity".to_vec())
            );
            assert_eq!(load_at(&path, &codec, "absent").unwrap(), None);

            delete_at(&path, &codec, "account_id_key_wrapped_u1").unwrap();
            assert_eq!(load_at(&path, &codec, "account_id_key_wrapped_u1").unwrap(), None);
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A plain READ of a pre-#882 file leaves it encrypted.
        #[test]
        fn a_read_migrates_the_plaintext_file_in_place() {
            let dir = std::env::temp_dir().join(format!("pollis-ks-readmig-{}", ulid::Ulid::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("keystore.pks");

            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"identity");
            let legacy = map_of(&[("account_id_key_wrapped_u1", encoded.as_str())]);
            std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();

            let codec = FileCodec::with_secret(MACHINE_A);
            assert_eq!(
                load_at(&path, &codec, "account_id_key_wrapped_u1").unwrap(),
                Some(b"identity".to_vec()),
                "the pre-migration key must still be readable"
            );

            let raw = std::fs::read(&path).unwrap();
            assert_eq!(&raw[..4], &FILE_MAGIC, "the read must have re-encrypted the file");
            assert_eq!(
                load_at(&path, &codec, "account_id_key_wrapped_u1").unwrap(),
                Some(b"identity".to_vec()),
                "and the key must survive the migration"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A failed migration write must not fail the READ.
        #[test]
        fn a_failed_migration_does_not_fail_the_read() {
            let dir = std::env::temp_dir().join(format!("pollis-ks-migfail-{}", ulid::Ulid::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("keystore.pks");

            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"identity");
            let bytes =
                serde_json::to_string(&map_of(&[("account_id_key_wrapped_u1", encoded.as_str())]))
                    .unwrap();
            std::fs::write(&path, &bytes).unwrap();

            let codec = FileCodec::that_cannot_seal();
            assert_eq!(
                load_at(&path, &codec, "account_id_key_wrapped_u1").unwrap(),
                Some(b"identity".to_vec()),
                "a read must never be failed by a migration problem"
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                bytes,
                "the unmigrated file must be left exactly as it was"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A temp dir plus the two store paths inside it.
        fn migration_dir(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
            let dir = std::env::temp_dir()
                .join(format!("pollis-ks-{tag}-{}", ulid::Ulid::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let new = dir.join("keystore.pks");
            let old = dir.join("dev-keystore.json");
            (dir, new, old)
        }

        /// #950(a): write-new → verify → unlink loses no key.
        #[test]
        fn the_store_is_renamed_without_losing_a_key() {
            let (dir, new, old) = migration_dir("rename");
            let codec = FileCodec::with_secret(MACHINE_A);

            let keys = map_of(&[
                ("account_id_key_wrapped_u1", "a2V5"),
                ("device_id_u1", "ZGV2"),
            ]);
            write_map_at(&old, &codec, &keys).unwrap();

            let resolved = resolve_store_at(&new, &old, &codec);

            assert_eq!(resolved, new, "the new name must become the store");
            assert!(new.exists(), "the new file must have been written");
            assert!(
                !old.exists(),
                "the misleading name must be gone once the new one is proven"
            );
            let (got, legacy) = read_map_at(&new, &codec).unwrap();
            assert_eq!(got, keys, "every key must survive the rename");
            assert!(!legacy, "the migrated file is sealed, not plaintext");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// If the new file can't be written, the old (only) copy stays the store.
        #[test]
        fn a_rename_that_cannot_complete_keeps_the_old_file() {
            let (dir, new, old) = migration_dir("rename-fail");
            let codec = FileCodec::with_secret(MACHINE_A);
            let keys = map_of(&[("account_id_key_wrapped_u1", "a2V5")]);
            write_map_at(&old, &codec, &keys).unwrap();
            let before = std::fs::read(&old).unwrap();

            // Fails at step 2; `that_cannot_seal()` would fail at step 1 instead.
            let broken = FileCodec::with_secret_but_cannot_seal(MACHINE_A);
            let resolved = resolve_store_at(&new, &old, &broken);

            assert_eq!(resolved, old, "a failed rename must keep serving the old file");
            assert!(!new.exists(), "no half-made new store may be left behind");
            assert_eq!(
                std::fs::read(&old).unwrap(),
                before,
                "the old file must be byte-identical"
            );
            assert_eq!(read_map_at(&old, &codec).unwrap().0, keys);
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A wrong-machine old file is not migrated, deleted or replaced.
        #[test]
        fn a_rename_never_touches_a_file_it_cannot_read() {
            let (dir, new, old) = migration_dir("rename-foreign");
            let codec_a = FileCodec::with_secret(MACHINE_A);
            write_map_at(&old, &codec_a, &map_of(&[("k", "v")])).unwrap();
            let before = std::fs::read(&old).unwrap();

            let codec_b = FileCodec::with_secret(MACHINE_B);
            let resolved = resolve_store_at(&new, &old, &codec_b);

            assert_eq!(resolved, old);
            assert!(!new.exists(), "an unreadable store must not spawn an empty one");
            assert_eq!(std::fs::read(&old).unwrap(), before);
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Crash before the unlink: the next start keeps the new file and finishes.
        #[test]
        fn a_rename_interrupted_before_the_unlink_finishes_on_the_next_start() {
            let (dir, new, old) = migration_dir("rename-resume");
            let codec = FileCodec::with_secret(MACHINE_A);

            let current = map_of(&[("account_id_key_wrapped_u1", "bmV3")]);
            let stale = map_of(&[("account_id_key_wrapped_u1", "b2xk")]);
            write_map_at(&new, &codec, &current).unwrap();
            write_map_at(&old, &codec, &stale).unwrap();

            let resolved = resolve_store_at(&new, &old, &codec);

            assert_eq!(resolved, new, "the completed new store wins");
            assert!(!old.exists(), "the unlink is retried and completes");
            assert_eq!(
                read_map_at(&new, &codec).unwrap().0,
                current,
                "the new store must not be overwritten by the stale one"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A fresh install has neither file and must simply use the new name.
        #[test]
        fn a_fresh_install_uses_the_new_name_directly() {
            let (dir, new, old) = migration_dir("rename-fresh");
            let codec = FileCodec::with_secret(MACHINE_A);
            assert_eq!(resolve_store_at(&new, &old, &codec), new);
            assert!(!new.exists(), "resolving must not create anything");
            assert!(!old.exists());
            std::fs::remove_dir_all(&dir).ok();
        }

        /// #950(c): a store that decrypts but doesn't parse is LEFT ALONE, and the failure stays stable.
        #[test]
        fn a_corrupt_store_is_never_moved_aside_or_started_over() {
            let (dir, path, _) = migration_dir("corrupt");
            let codec = FileCodec::with_secret(MACHINE_A);

            std::fs::write(&path, codec.encode("this is not the store").unwrap()).unwrap();
            let before = std::fs::read(&path).unwrap();

            let err = read_map_at(&path, &codec).unwrap_err();
            assert!(
                err.to_string().contains("left untouched"),
                "the error must say the file was kept, got: {err}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a corrupt store must not be moved, renamed or truncated"
            );
            assert!(
                std::fs::read_dir(&dir).unwrap().count() == 1,
                "no `.bad-<ts>` sidecar may be created"
            );

            // The NEXT operation must still refuse, not create a fresh store.
            let store_err = store_at(&path, &codec, "k".into(), "dg==".into()).unwrap_err();
            assert!(store_err.to_string().contains("unreadable"));
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a corrupt store must not be replaced by a fresh empty one"
            );
            assert!(load_at(&path, &codec, "k").is_err());
            assert!(delete_at(&path, &codec, "k").is_err());
            std::fs::remove_dir_all(&dir).ok();
        }

        /// #950(b): the namespace rule over all four shapes.
        #[test]
        fn namespace_matching_is_exact_in_every_shape() {
            use super::super::key_is_in_namespace as in_ns;

            assert!(in_ns("", "account_id_key_wrapped_u1"));
            assert!(
                !in_ns("", "dev2:account_id_key_wrapped_u1"),
                "a second instance's key must not pin the default namespace"
            );

            assert!(in_ns("DEV:", "DEV:db_key_u1"));
            assert!(!in_ns("DEV:", "db_key_u1"));
            assert!(!in_ns("DEV:", "dev2:DEV:db_key_u1"));

            assert!(in_ns("dev2:", "dev2:db_key_u1"));
            assert!(!in_ns("dev2:", "dev3:db_key_u1"));
            assert!(!in_ns("dev2:", "db_key_u1"));
            assert!(in_ns("dev2:DEV:", "dev2:DEV:db_key_u1"));
            assert!(!in_ns("dev2:DEV:", "dev3:DEV:db_key_u1"));

            // The rule agrees with the generator it mirrors.
            assert!(super::super::is_current_namespace(&super::super::namespaced(
                "account_id_key_wrapped_u1"
            )));
            assert!(!super::super::is_current_namespace(&format!(
                "dev2:{}",
                super::super::namespaced("account_id_key_wrapped_u1")
            )));
        }

        /// Only this namespace's keys count, so a second dev instance can't pin the first's backend.
        #[test]
        fn only_this_namespace_s_keys_pin_the_backend() {
            let mine = super::super::namespaced("account_id_key_wrapped_u1");
            let theirs = format!("dev2:{mine}");

            assert!(!map_holds_current_namespace_keys(&HashMap::new()));
            assert!(map_holds_current_namespace_keys(&map_of(&[(
                mine.as_str(),
                "a2V5"
            )])));
            assert!(
                !map_holds_current_namespace_keys(&map_of(&[(theirs.as_str(), "a2V5")])),
                "another instance's keys must not keep this one off the keychain"
            );
        }

        /// Pure in (secret, salt).
        #[test]
        fn kek_derivation_binds_both_secret_and_salt() {
            use super::super::machine_kek::derive_kek;

            let salt1 = [1u8; 16];
            let salt2 = [2u8; 16];
            assert_eq!(*derive_kek(MACHINE_A, &salt1), *derive_kek(MACHINE_A, &salt1));
            assert_ne!(*derive_kek(MACHINE_A, &salt1), *derive_kek(MACHINE_A, &salt2));
            assert_ne!(*derive_kek(MACHINE_A, &salt1), *derive_kek(MACHINE_B, &salt1));
        }
    }
}

// OS keychain backend (`os-keystore`); whether it is used is decided at runtime by `backend::kind()`.

#[cfg(feature = "os-keystore")]
mod keychain_backend {
    use super::namespaced;
    use crate::error::{Error, Result};
    use keyring::Entry;

    const SERVICE: &str = "pollis";

    /// Read-only probe for "is there a usable credential store on this host".
    ///
    /// Reading a missing entry exercises the whole path (D-Bus on Linux) with no write or prompt.
    ///
    /// `NoEntry` is healthy; anything else means the keychain is unusable.
    pub fn available() -> bool {
        match Entry::new(SERVICE, &namespaced("backend-probe")).map(|e| e.get_password()) {
            Ok(Ok(_)) | Ok(Err(keyring::Error::NoEntry)) => true,
            Ok(Err(e)) => {
                crate::diag!("[keystore] OS keychain unavailable ({e})");
                false
            }
            Err(e) => {
                crate::diag!("[keystore] OS keychain unavailable ({e})");
                false
            }
        }
    }

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

/// Which at-rest backend this process uses.
///
/// **No `Plaintext` variant** (#882): only the two acceptable outcomes are representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// The OS keychain; preferred wherever it exists.
    OsKeychain,
    /// AES-256-GCM file under a machine-bound (desktop) or OS-held (mobile) key.
    EncryptedFile,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OsKeychain => write!(f, "OS keychain"),
            Self::EncryptedFile => write!(f, "encrypted file"),
        }
    }
}

/// Whether this build has the OS keychain backend compiled in at all.
///
/// `pollis-tui`'s tests assert this, so the keychain can't silently be dropped again (#879).
pub const fn os_keychain_compiled() -> bool {
    cfg!(feature = "os-keystore")
}

mod backend {
    use super::{file_backend, BackendKind};
    use crate::error::Result;
    use std::sync::OnceLock;

    static KIND: OnceLock<BackendKind> = OnceLock::new();

    /// Decided ONCE per process and then frozen.
    ///
    /// Frozen so a flaky keychain can't split slots across two stores (#184).
    fn detect() -> BackendKind {
        // Mobile: the file is the store, sealed by the OS key.
        if cfg!(any(target_os = "android", target_os = "ios")) {
            return BackendKind::EncryptedFile;
        }
        // Debug builds use the file, so dev never prompts.
        if cfg!(debug_assertions) {
            return BackendKind::EncryptedFile;
        }
        keychain_or_file()
    }

    /// #882: one binary uses the keychain on a desktop and the encrypted file on a headless server.
    #[cfg(feature = "os-keystore")]
    fn keychain_or_file() -> BackendKind {
        if !super::keychain_backend::available() {
            crate::diag!(
                "[keystore] no usable OS keychain on this host; \
                 falling back to the machine-bound encrypted file store"
            );
            return BackendKind::EncryptedFile;
        }

        // Not enough that a keychain answers (#950): whoever holds this namespace's keys keeps them; if both do, the file (newer) wins.
        if super::file_backend::holds_current_namespace_keys() {
            crate::diag!(
                "[keystore] an OS keychain is available, but this install's keys \
                 already live in the encrypted file store; staying there so the \
                 device is not treated as new (#950)"
            );
            return BackendKind::EncryptedFile;
        }

        BackendKind::OsKeychain
    }

    #[cfg(not(feature = "os-keystore"))]
    fn keychain_or_file() -> BackendKind {
        BackendKind::EncryptedFile
    }

    /// The probe blocks (D-Bus round trip), so it runs on the blocking pool.
    pub async fn kind() -> BackendKind {
        if let Some(k) = KIND.get() {
            return *k;
        }
        let detected = tokio::task::spawn_blocking(detect)
            .await
            .unwrap_or(BackendKind::EncryptedFile);
        *KIND.get_or_init(|| detected)
    }

    pub async fn store(key: &str, value: &[u8]) -> Result<()> {
        match kind().await {
            BackendKind::EncryptedFile => file_backend::store(key, value).await,
            #[cfg(feature = "os-keystore")]
            BackendKind::OsKeychain => super::keychain_backend::store(key, value).await,
            #[cfg(not(feature = "os-keystore"))]
            BackendKind::OsKeychain => unreachable!("no keychain backend compiled in"),
        }
    }

    pub async fn load(key: &str) -> Result<Option<Vec<u8>>> {
        match kind().await {
            BackendKind::EncryptedFile => file_backend::load(key).await,
            #[cfg(feature = "os-keystore")]
            BackendKind::OsKeychain => super::keychain_backend::load(key).await,
            #[cfg(not(feature = "os-keystore"))]
            BackendKind::OsKeychain => unreachable!("no keychain backend compiled in"),
        }
    }

    pub async fn delete(key: &str) -> Result<()> {
        match kind().await {
            BackendKind::EncryptedFile => file_backend::delete(key).await,
            #[cfg(feature = "os-keystore")]
            BackendKind::OsKeychain => super::keychain_backend::delete(key).await,
            #[cfg(not(feature = "os-keystore"))]
            BackendKind::OsKeychain => unreachable!("no keychain backend compiled in"),
        }
    }
}

/// Which backend this process settled on. Diagnostic only.
pub async fn backend_kind() -> BackendKind {
    backend::kind().await
}

/// Secret storage: [`OsKeystore`] in production, [`InMemoryKeystore`] per [`TestClient`] in tests.
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

/// Delegates to the backend chosen once per process; neither writes plaintext.
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

/// Per-[`TestClient`] in-memory keystore. Never use in production.
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

/// The resolved per-user slot name for THIS process.
///
/// For `tests/keystore_at_rest.rs`, so the naming rule isn't hand-copied.
pub fn slot_name(key: &str, user_id: &str) -> String {
    namespaced(&format!("{key}_{user_id}"))
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

    /// GUARD: the chosen backend always encrypts; a plaintext variant would break this.
    #[tokio::test]
    async fn guard_selected_backend_is_never_plaintext() {
        let kind = backend_kind().await;
        assert!(
            matches!(kind, BackendKind::OsKeychain | BackendKind::EncryptedFile),
            "the only representable backends must both encrypt at rest, got {kind}"
        );
    }

    /// The choice is frozen for the process (#184).
    #[tokio::test]
    async fn guard_backend_choice_is_stable_within_a_process() {
        let first = backend_kind().await;
        for _ in 0..5 {
            assert_eq!(backend_kind().await, first);
        }
    }

    /// GUARD: a debug build never reaches for the OS keychain.
    ///
    /// Only discriminating with `os-keystore`; gated since `--release` takes the other branch.
    #[cfg(debug_assertions)]
    #[tokio::test]
    async fn debug_builds_use_the_file_backend() {
        assert_eq!(backend_kind().await, BackendKind::EncryptedFile);
    }

    #[test]
    fn kek_envelope_roundtrip() {
        let key = [7u8; 32];
        let blob = kek_envelope::seal_with(&key, b"identity keys + session").unwrap();
        assert_eq!(
            kek_envelope::unseal_with(&key, &blob).unwrap(),
            b"identity keys + session"
        );
    }

    #[test]
    fn kek_envelope_blob_layout_is_iv12_ct_tag16() {
        // Pins the layout shared with android_kek: iv(12) || ct || tag(16).
        let key = [1u8; 32];
        let pt = b"0123456789";
        let blob = kek_envelope::seal_with(&key, pt).unwrap();
        assert_eq!(blob.len(), 12 + pt.len() + 16);
        let blob2 = kek_envelope::seal_with(&key, pt).unwrap();
        assert_ne!(blob[..12], blob2[..12], "IV must be random per seal");
    }

    #[test]
    fn kek_envelope_rejects_wrong_key_and_tamper() {
        let key = [2u8; 32];
        let blob = kek_envelope::seal_with(&key, b"secret").unwrap();
        assert!(kek_envelope::unseal_with(&[3u8; 32], &blob).is_err());
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(kek_envelope::unseal_with(&key, &tampered).is_err());
        assert!(kek_envelope::unseal_with(&key, &blob[..8]).is_err());
    }
}
