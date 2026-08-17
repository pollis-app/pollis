use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// When an explicit data dir is chosen (a second dev instance's
/// `POLLIS_DATA_DIR`, or a test rig's [`crate::db::local::set_data_dir`]),
/// namespace keyring entries so multiple instances don't stomp each other's
/// session/identity keys. A build that chose no directory — every shipped
/// desktop install — is unaffected and keeps its unprefixed keys.
fn namespaced(key: &str) -> String {
    format!("{}{key}", namespace_prefix())
}

/// The prefix [`namespaced`] puts in front of every key this process touches.
///
/// Split out so the backend-selection probe can ask "does the file store hold
/// keys belonging to **me**" without re-deriving the rule (#950). It is `""` for
/// a shipped desktop release, `"DEV:"` for a debug build, and gains a
/// `"{label}:"` segment when an explicit data dir is chosen.
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
/// The prefix test alone is not enough: every real slot name (`db_key`,
/// `account_id_key_wrapped`, `pin_meta`, `device_id`, `session`) is free of
/// `:`, and the unprefixed release namespace would otherwise match a second
/// instance's `dev2:db_key_u1` too. Requiring the remainder to carry no further
/// `:` segment makes the match exact in every one of the four namespace shapes.
#[cfg(any(feature = "os-keystore", test))]
fn is_current_namespace(stored_key: &str) -> bool {
    key_is_in_namespace(&namespace_prefix(), stored_key)
}

/// The pure rule behind [`is_current_namespace`], split out so it can be tested
/// against all four namespace shapes without touching the process-wide data dir.
#[cfg(any(feature = "os-keystore", test))]
fn key_is_in_namespace(prefix: &str, stored_key: &str) -> bool {
    match stored_key.strip_prefix(prefix) {
        Some(rest) => !rest.contains(':'),
        None => false,
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

// ── iOS: encrypt the keystore file at rest under a Keychain-held key ─────────
//
// Mirror of `android_kek` for iOS (issue #185 Section A): the file-backed
// store's bytes are AES-256-GCM-encrypted under a 32-byte master key held in
// the iOS **Keychain** (a generic-password item). Keychain items are encrypted
// at rest by the OS under Secure-Enclave-protected class keys;
// `AccessibleAfterFirstUnlockThisDeviceOnly` keeps the key out of ALL backups
// (iCloud + local) and available across relaunches — so identity + session
// survive a relaunch while the on-disk ciphertext is useless off-device.
// Same blob layout as Android: iv(12) || gcm-ciphertext(+16 tag).
//
// Unlike AndroidKeyStore, the DEK crosses into process memory (Keychain
// generic-password items are readable by the owning app). That matches the
// threat model both platforms defend against here — off-device file theft via
// backups or sandbox reads — since on either platform an on-device attacker
// with app privileges can simply call unseal. Secure-Enclave-resident,
// non-extractable keys (ECIES) are a possible hardening later; they don't work
// in the iOS simulator, which is what the dev loop runs on.
//
// The AES envelope itself is platform-neutral Rust (`kek_envelope` below) so
// the seal/unseal round-trip and blob layout are unit-tested on every host.
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

    /// Process-local cache of the master key. Also serializes get-or-create so
    /// two concurrent first writes can't each mint a different key (the second
    /// `SecItemAdd` silently *updates* on duplicate, which would orphan the
    /// first writer's ciphertext).
    static MASTER: Mutex<Option<[u8; 32]>> = Mutex::new(None);

    fn keychain_err(ctx: &str, err: security_framework::base::Error) -> Error {
        Error::Keystore(format!("ios keychain {ctx}: {err}"))
    }

    /// Get-or-create the 32-byte master key in the iOS Keychain.
    fn master_key() -> Result<[u8; 32]> {
        let mut guard = MASTER
            .lock()
            .map_err(|_| Error::Keystore("ios kek mutex poisoned".into()))?;
        if let Some(k) = *guard {
            return Ok(k);
        }

        let key: [u8; 32] = match get_generic_password(SERVICE, ACCOUNT) {
            Ok(bytes) => bytes.try_into().map_err(|_| {
                // A wrong-sized item means the entry was tampered with or
                // written by something else. Refuse — regenerating would orphan
                // every sealed byte on disk (identity keys included).
                Error::Keystore("ios keychain kek has unexpected size; refusing to overwrite".into())
            })?,
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => {
                let mut fresh = [0u8; 32];
                rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut fresh);
                let mut opts = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
                let protection = SecAccessControl::create_with_protection(
                    Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
                    0,
                )
                .map_err(|x| keychain_err("access control", x))?;
                opts.set_access_control(protection);
                set_generic_password_options(&fresh, opts)
                    .map_err(|x| keychain_err("store kek", x))?;
                fresh
            }
            Err(e) => return Err(keychain_err("read kek", e)),
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

// AES-256-GCM envelope shared by the iOS path and, since #882, by the desktop
// file store. Platform-neutral on purpose: compiled (and unit-tested) on every
// target so the blob layout — iv(12) || ciphertext(+16 tag), byte-compatible
// with `android_kek`'s Java output — is pinned by host tests instead of only
// exercised on a device. Android is the one target that never calls it (its
// seal/unseal runs inside the JVM against a non-exportable key).
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


// ── Desktop: the machine-bound key that encrypts the file store ─────────────
//
// #882. The file store below used to be plaintext JSON on desktop. Mobile
// already sealed the same file under an OS-held key (`android_kek` /
// `ios_kek`); desktop was the outlier, and #879 showed the cost of leaving it
// that way — the only way to keep the shipped CLI's keys off disk in the clear
// was to require a secret-service, which a headless box (the CLI's likely home)
// does not have.
//
// WHAT THIS DEFENDS AGAINST — be precise, because the honest answer is narrow.
//
// The realistic threat an at-rest file cipher addresses is **the file leaving
// the machine**: a home-directory backup, an `scp` of the data dir, a synced
// folder, a stolen or resold disk that is later read on other hardware, a
// container image built over a live data dir. The KEK is derived from a secret
// that lives OUTSIDE the data directory and does not travel with it — the
// systemd/D-Bus machine ID on Linux, IOPlatformUUID on macOS, MachineGuid on
// Windows — so the copied bytes are undecryptable on any other machine.
//
// WHAT IT DOES NOT DEFEND AGAINST — equally important:
//
//   * A local attacker running as the SAME UID. Whatever this process can
//     derive to decrypt, that attacker can derive too. Nothing done in
//     userspace changes that; the OS keychain backend is what raises this bar,
//     which is why it stays the preferred backend wherever one exists.
//   * A FULL-DISK image or whole-VM snapshot. `/etc/machine-id` is on the same
//     disk as the keystore, and is world-readable. Machine binding defeats
//     *selective* exfiltration of the data dir, not an image of everything.
//   * Forensic recovery of the pre-migration plaintext. Replacing the file
//     atomically unlinks the old inode; its blocks are not securely erased,
//     and no userspace API can promise that on a modern SSD/CoW filesystem.
//
// WHY THE PIN IS NOT MIXED INTO THIS KEK. It is tempting — the PIN is the one
// secret the machine does not hold. Two reasons it is the wrong layer:
//
//   1. It cannot work. The keystore is read BEFORE the PIN exists: boot reads
//      `pin_meta_{uid}` (to decide whether to show "enter PIN" or "set PIN")
//      and `device_id_{uid}`. A PIN-derived file KEK would make the file
//      unreadable until after the thing stored in the file had been read.
//   2. It is already done, one layer in. Since the PIN work (#194) the actual
//      secrets in this file — `db_key_wrapped_{uid}`, `account_id_key_wrapped_
//      {uid}` — are Argon2id(64 MiB, t=3) + XChaCha20-Poly1305 ciphertext under
//      a PIN-derived KEK (`commands/pin.rs`). Wrapping them a second time under
//      the same PIN adds nothing.
//
// So the two layers are complementary, and each covers the other's gap:
//
//   * The PIN layer alone is weak against an exfiltrated file: 4 digits is
//     10^4 candidates, and at the tuned ~250 ms/guess a full offline sweep is
//     roughly 40 minutes on one core. That is the hole #882 exists to close.
//   * The machine-bound layer alone is weak against a same-UID local attacker.
//   * Together, an attacker needs the machine's identity AND the PIN, and the
//     file on its own is inert.
//
// A TPM (or Secure Enclave) would genuinely improve the first bullet — a
// sealed key is absent from every backup and every disk image. It is NOT used
// here: `tss-esapi` is a heavy C dependency on `tpm2-tss`, needs a TPM present
// AND a resource manager reachable by the invoking user, and would need three
// separate platform integrations. Half-integrating one would mean a silent
// fallback path that looks stronger than it is. Recorded as a deliberate
// omission rather than pretended.
//
// The KEK is HKDF-SHA256, not Argon2id: the machine ID is a 128-bit random
// value, so there is nothing for a slow KDF to slow down. The Argon2id cost
// belongs on the PIN, and that is where it already is.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod machine_kek {
    use crate::error::{Error, Result};
    use hkdf::Hkdf;
    use sha2::Sha256;
    use std::sync::OnceLock;
    use zeroize::Zeroizing;

    /// HKDF domain separator. Bump the suffix if the derivation ever changes
    /// meaning — the file's magic byte carries the format version separately.
    const HKDF_INFO: &[u8] = b"pollis-keystore-file-kek-v1";

    /// Per-file HKDF salt length. Fresh on every write, stored in the clear in
    /// the file header, so two Pollis installs on one machine (a second dev
    /// instance under `POLLIS_DATA_DIR`) never share a KEK.
    pub const SALT_LEN: usize = 16;

    pub const KEK_LEN: usize = 32;

    /// Escape hatch for hosts with no machine ID of their own — some minimal
    /// containers ship an empty `/etc/machine-id`. Also what the unit tests
    /// would use if they needed the real accessor; they drive the pure
    /// derivation instead. Setting this to a fixed value on every host would
    /// reduce the file to "encrypted under a public constant", so it is
    /// documented as a last resort in `docs/run-it-yourself.md`, not a knob.
    const MACHINE_ID_ENV: &str = "POLLIS_KEYSTORE_MACHINE_ID";

    static SECRET: OnceLock<Option<Zeroizing<Vec<u8>>>> = OnceLock::new();

    /// The machine-bound secret, read once per process.
    ///
    /// Hard error when no source yields one. The alternative — falling back to
    /// a constant, or to a random file living next to the keystore — would
    /// either be encryption in name only or would add a way to permanently
    /// lose an identity key by deleting one extra file. Neither is acceptable,
    /// so an exotic host is told exactly which variable to set.
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

    /// systemd's machine ID, falling back to D-Bus's. Both are a 128-bit
    /// host-unique value that survives reboots and does not travel with a copy
    /// of the home directory.
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

    /// IOPlatformUUID — the hardware UUID the OS reports for this Mac. Read via
    /// `ioreg` rather than linking IOKit: this path only runs when the macOS
    /// Keychain is unavailable (in practice, dev builds), so a one-off
    /// subprocess at startup is cheaper than another native dependency.
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

    /// MachineGuid — written by Windows at install time. `/reg:64` so a 32-bit
    /// process is not silently redirected to the WOW6432Node view, which holds
    /// a different value. `creation_flags(CREATE_NO_WINDOW)` keeps a console
    /// from flashing over the app.
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

    /// Pure derivation, split out from the accessor so the tests can exercise
    /// "same machine" and "file copied to another machine" without touching —
    /// or needing — the real host identity.
    pub fn derive_kek(machine_secret: &[u8], salt: &[u8; SALT_LEN]) -> Zeroizing<[u8; KEK_LEN]> {
        let hk = Hkdf::<Sha256>::new(Some(salt), machine_secret);
        let mut out = Zeroizing::new([0u8; KEK_LEN]);
        // HKDF-SHA256 only fails for an output longer than 255*32 bytes.
        hk.expand(HKDF_INFO, &mut *out)
            .expect("32-byte HKDF output is always in range");
        out
    }
}

// ── File-backed keystore: an encrypted file (no keychain, no OS prompts) ────
//
// Selected for debug builds (no OS prompts during dev/test), whenever the
// `os-keystore` feature is off, and — since #882 — at RUNTIME whenever a
// release build finds no usable OS keychain (the headless-server case that
// #879 traded away). On mobile it is the only backend.
//
// The file's bytes are ciphertext on every platform. There is no plaintext
// encode path left in this module to reach, by feature, by cfg, or by mistake.

mod file_backend {
    use super::namespaced;
    use crate::error::{Error, Result};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// The store's real name, since #950.
    ///
    /// `dev-keystore.json` was a fossil from when this file was genuinely
    /// debug-only. After #882 it is the PRODUCTION store on every headless
    /// install, and it is not JSON — it is `PKS\x01` + salt + AES-256-GCM
    /// ciphertext. A name that says "dev" and "json" about a file that is
    /// neither is the kind of thing that gets someone to delete it.
    fn store_path() -> PathBuf {
        // The same directory the local DB and `accounts.json` resolve to. This
        // used to be a hand-copied per-OS duplicate of `dirs_path()`; the two
        // agreed on every branch, so the copy bought nothing and could only ever
        // drift — a keystore looking in a different directory from the database
        // it unlocks is exactly the failure that would not show up until a
        // platform default changed on one side.
        crate::db::local::dirs_path().join("keystore.pks")
    }

    /// Where the store lived before #950. Read (and migrated away from) but
    /// never written.
    fn legacy_store_path() -> PathBuf {
        crate::db::local::dirs_path().join("dev-keystore.json")
    }

    /// Decide which file this process operates on, performing the #950 rename
    /// if it has not happened yet.
    ///
    /// The rename is a delete of live key material, which is why #882 declined
    /// to do it inline. So it runs as an explicit three-step, in the order that
    /// makes every interruption survivable:
    ///
    ///   1. **write the new file** (atomically, via `write_map_at` — tempfile,
    ///      fsync, rename);
    ///   2. **read it back** and check it reproduces the same map;
    ///   3. **only then unlink the old one.**
    ///
    /// A crash between 1 and 3 leaves both files present, and the next start
    /// takes the "new file exists" branch and simply retries the unlink — so the
    /// window is idempotent rather than lossy. The one state that must never
    /// exist is "old file gone, new file bad", and no ordering here can produce
    /// it.
    ///
    /// Every failure falls back to the legacy path rather than erroring. That is
    /// deliberate: a rename is housekeeping, and housekeeping must never be the
    /// reason a user cannot reach their keys. An undecryptable or corrupt file
    /// therefore surfaces through the normal read path, with the normal message,
    /// exactly as it did before this function existed.
    fn resolve_store(codec: &FileCodec) -> PathBuf {
        resolve_store_at(&store_path(), &legacy_store_path(), codec)
    }

    /// [`resolve_store`] with both paths supplied, so the tests drive the SAME
    /// migration the runtime does against a temp dir.
    fn resolve_store_at(new: &Path, old: &Path, codec: &FileCodec) -> PathBuf {
        let new = new.to_path_buf();
        let old = old.to_path_buf();

        if new.exists() {
            // Step 3 retry: a previous run wrote and verified `new` but did not
            // manage to unlink `old`. `new` is authoritative either way, so the
            // stale copy is redundant key material sitting on disk under a
            // misleading name — remove it, but never at the cost of failing.
            if old.exists() {
                match std::fs::remove_file(&old) {
                    Ok(()) => eprintln!(
                        "[keystore] removed the superseded {} (#950)",
                        old.display()
                    ),
                    Err(e) => eprintln!(
                        "[keystore] could not remove the superseded {}: {e}",
                        old.display()
                    ),
                }
            }
            return new;
        }

        if !old.exists() {
            // Fresh install, or one already migrated and tidied.
            return new;
        }

        // 1. Read the old store. A failure here is "wrong machine" or "corrupt";
        //    both must reach the caller through the ordinary path so the user
        //    gets the ordinary (accurate) error, and neither may delete anything.
        let (map, _) = match read_map_at(&old, codec) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "[keystore] cannot read {} ({e}); leaving it exactly where it is",
                    old.display()
                );
                return old;
            }
        };

        // 2. Write the new file, then prove it reads back as the same map.
        if let Err(e) = write_map_at(&new, codec, &map) {
            eprintln!(
                "[keystore] could not write {} ({e}); staying on {}",
                new.display(),
                old.display()
            );
            return old;
        }
        match read_map_at(&new, codec) {
            Ok((got, false)) if got == map => {}
            other => {
                // Do NOT unlink the old file, and do not adopt the new one.
                // Deliberately not printing `other` — it can hold decrypted key
                // material.
                let _ = other;
                eprintln!(
                    "[keystore] {} did not read back correctly; staying on {}",
                    new.display(),
                    old.display()
                );
                let _ = std::fs::remove_file(&new);
                return old;
            }
        }

        // 3. The new file is real and complete. The old one may go.
        match std::fs::remove_file(&old) {
            Ok(()) => eprintln!(
                "[keystore] renamed the keystore to {} (#950)",
                new.display()
            ),
            Err(e) => eprintln!(
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
    /// The header needs no AEAD associated data: the salt is an *input* to the
    /// KEK, so substituting it changes the key and the GCM tag check fails.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    const FILE_MAGIC: [u8; 4] = *b"PKS\x01";

    /// What a successful read found on disk. `LegacyPlaintext` is the
    /// pre-#882 desktop file; it is readable exactly so that upgraders are
    /// never locked out, and the first write that follows replaces it.
    enum Decoded {
        Encrypted(String),
        #[cfg_attr(
            any(target_os = "android", target_os = "ios"),
            allow(dead_code)
        )]
        LegacyPlaintext(String),
    }

    /// The at-rest codec for the file store. Desktop derives a machine-bound
    /// KEK; mobile defers to the OS-held key. Constructed once per operation
    /// and passed down so tests can supply an explicit secret.
    struct FileCodec {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        machine_secret: Vec<u8>,
        /// Test-only: simulate "this host has no derivable KEK" so the real
        /// `write_map_at` can be driven through its seal-failure path.
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

        /// Can DECODE `machine_secret`'s files but cannot encode. The
        /// distinction matters for the #950 rename: `that_cannot_seal` above
        /// also carries the wrong secret, so a test using it fails at the READ
        /// and never reaches the write it meant to exercise.
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
                // A failure here is "wrong machine", not "corrupt": the caller
                // must surface it and leave the file alone. Silently starting
                // over would orphan the identity key sitting in these bytes.
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

            // Pre-#882 desktop file: plaintext JSON. Read it so upgraders are
            // never locked out; the next write re-encrypts in place.
            let json = String::from_utf8(raw.to_vec())
                .map_err(|e| Error::Keystore(format!("keystore utf8: {e}")))?;
            Ok(Decoded::LegacyPlaintext(json))
        }

        // Mobile: the same file, sealed under a key the OS holds. Unchanged by
        // #882 — this is the shape desktop is being brought in line with.
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

    /// Reads the store. The bool is "this file is still pre-#882 plaintext",
    /// which the callers use to trigger the in-place re-encryption.
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
        // A decode failure propagates untouched — see `FileCodec::decode`. Only
        // a *parse* failure after a successful decrypt is treated as corruption.
        let (data, legacy) = match codec.decode(&raw)? {
            Decoded::Encrypted(json) => (json, false),
            Decoded::LegacyPlaintext(json) => (json, true),
        };
        match serde_json::from_str(&data) {
            Ok(m) => Ok((m, legacy)),
            Err(parse_err) => {
                // #950 — the file is LEFT WHERE IT IS.
                //
                // This used to rename the file to `dev-keystore.bad-{ts}.json`
                // and return an error. That reads like a careful backup, and it
                // is not: the very next operation finds no store at all, treats
                // the device as brand new, and the first write establishes an
                // empty one. The user is silently signed out, and their account
                // identity key — unrecoverable without the one-time Secret Key —
                // is orphaned in a file named after a timestamp that nothing
                // will ever read again. "Start over" is the wrong default for
                // material that cannot be regenerated.
                //
                // Leaving the bytes in place converts that into a loud, stable,
                // recoverable failure: every subsequent operation reports the
                // same error against the same file, the content is still there
                // for a human (or a later format-tolerant reader) to salvage,
                // and nothing overwrites it. The deliberate escape hatch is
                // `commands::auth::wipe_local_data` — the login screen's "wipe
                // this computer" — which is a decision the user makes, not one
                // a parse error makes for them.
                //
                // Note this is only reachable AFTER a successful decrypt (#882),
                // so it means genuinely malformed plaintext, never wrong-machine
                // ciphertext.
                eprintln!(
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

    /// The atomic-write staging file: the store's own name with `.tmp`
    /// appended, so it is unmistakably a sibling of this exact store rather
    /// than of any other file that happens to share a stem.
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
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Keystore(format!("create dir: {e}")))?;
        }
        let json = serde_json::to_string(map)
            .map_err(|e| Error::Keystore(format!("serialize: {e}")))?;
        // Ciphertext on every platform. Encoding happens BEFORE the old file is
        // touched, so a codec failure cannot leave a half-written store.
        let data = codec.encode(&json)?;

        // Atomic write: tempfile + fsync + rename. A crash before the rename
        // leaves the old file intact. Without this, a crash mid-write turned
        // the keystore into a zero-byte file and bounced every user to OTP.
        //
        // This is also what makes the plaintext→encrypted migration safe: the
        // encrypted bytes only become the store in one atomic step, so every
        // interruption leaves EITHER the complete old plaintext file OR the
        // complete new encrypted one, and never a partial mix of the two.
        let tmp = tmp_path(path);
        {
            let mut f = std::fs::File::create(&tmp)
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

    // The three operations, factored to take an explicit path + codec so the
    // tests below drive the SAME code the runtime does — including the
    // migration branch — against a temp dir, with no env-var juggling and no
    // dependence on the host's real machine identity.

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

        // Opportunistic migration: a read-only session must not leave pre-#882
        // plaintext lying on disk. Best-effort on purpose — a failure here must
        // never fail the read, because the read is how the user gets to their
        // keys at all.
        if legacy {
            match write_map_at(path, codec, &map) {
                Ok(()) => eprintln!(
                    "[keystore] re-wrote the plaintext keystore encrypted at rest (#882)"
                ),
                Err(e) => eprintln!(
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
            // No codec, no write. This is the invariant that makes "plaintext
            // never" true: a host we cannot derive a KEK for gets a loud error,
            // not a plaintext fallback.
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

    /// The predicate half of [`holds_current_namespace_keys`], over an
    /// already-read map.
    #[cfg(any(feature = "os-keystore", test))]
    fn map_holds_current_namespace_keys(map: &HashMap<String, String>) -> bool {
        map.keys().any(|k| super::is_current_namespace(k))
    }

    /// Does the file store already hold keys belonging to THIS namespace?
    ///
    /// The question [`super::backend`] asks before handing a box that has just
    /// grown a Secret Service over to the keychain (#950). Answering it needs
    /// the file read anyway, so it reuses the ordinary read path — including the
    /// #950 rename — rather than sniffing the raw bytes.
    ///
    /// **An unreadable store answers `false`, deliberately.** If the file cannot
    /// be decrypted or parsed then staying on it buys nothing: every read would
    /// fail. The keychain is empty, which costs one re-enrolment and leaves the
    /// file bytes on disk untouched for salvage — strictly better than a device
    /// that cannot start at all.
    #[cfg(feature = "os-keystore")]
    pub fn holds_current_namespace_keys() -> bool {
        let Ok(codec) = FileCodec::detect() else {
            return false;
        };
        match read_map_at(&resolve_store(&codec), &codec) {
            Ok((map, _)) => map_holds_current_namespace_keys(&map),
            Err(e) => {
                eprintln!(
                    "[keystore] could not inspect the file store while choosing a \
                     backend ({e}); treating it as empty"
                );
                false
            }
        }
    }

    // ── Tests ───────────────────────────────────────────────────────────────
    //
    // Everything here drives the codec and the on-disk file directly with an
    // EXPLICIT machine secret, so nothing depends on (or disturbs) the host's
    // real identity, and "the file was copied to another machine" is a first-
    // class case rather than something only reachable on real hardware.
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

        /// GUARD (#882) — the whole point of the ticket. If someone reinstates
        /// a plaintext encode path, this fails loudly.
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
            // Not "contains no '{'" — ciphertext is random and will hit 0x7b
            // by chance. The load-bearing checks are the two above and the
            // parse below; this one pins that it is not a JSON document.
            assert_ne!(raw[0], b'{', "the file must not start a JSON object");
            assert!(
                serde_json::from_slice::<HashMap<String, String>>(&raw).is_err(),
                "the file must not parse as the plaintext store"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// The machine binding is the property that makes an exfiltrated file
        /// useless. Same bytes, different host secret → refuses, and says so.
        #[test]
        fn file_copied_to_another_machine_does_not_decrypt() {
            let codec_a = FileCodec::with_secret(MACHINE_A);
            let codec_b = FileCodec::with_secret(MACHINE_B);

            let raw = codec_a.encode(r#"{"k":"v"}"#).unwrap();
            assert!(matches!(
                codec_a.decode(&raw).unwrap(),
                Decoded::Encrypted(_)
            ));

            // Matched rather than `unwrap_err`d on purpose: `Decoded` holds
            // decrypted key material and must never gain a `Debug` impl.
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

            // Flip a bit in the ciphertext body.
            let mut body_flip = raw.clone();
            let last = body_flip.len() - 1;
            body_flip[last] ^= 1;
            assert!(codec.decode(&body_flip).is_err(), "GCM tag must catch a body flip");

            // Flip a bit in the salt: the KEK changes, so the tag fails too.
            let mut salt_flip = raw.clone();
            salt_flip[5] ^= 1;
            assert!(codec.decode(&salt_flip).is_err(), "header must be bound to the body");

            // Truncated to just the header.
            assert!(codec.decode(&raw[..20]).is_err(), "truncation must not panic");

            // Magic present but nothing after it.
            assert!(codec.decode(&FILE_MAGIC).is_err(), "short header must error");
        }

        /// A file we cannot decrypt must be left exactly as it was — the bytes
        /// are someone's identity key, and "start over" would orphan them.
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

        /// The upgrade path: a pre-#882 plaintext file is readable, and the
        /// next write replaces it with ciphertext holding the SAME keys.
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

            // The re-write the load path performs.
            write_map_at(&path, &codec, &got).unwrap();

            let raw = std::fs::read(&path).unwrap();
            assert_eq!(&raw[..4], &FILE_MAGIC, "migrated file must be sealed");
            let (after, still_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(after, legacy, "migration must preserve every key");
            assert!(!still_legacy, "migration must not need to run twice");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// A crash between "temp file written" and "rename" — the only window
        /// the atomic write has. The original file must still hold every key,
        /// and re-running the migration must succeed.
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

            // Exactly what a crash before the rename leaves behind: a partial
            // temp file next to an intact original.
            let codec = FileCodec::with_secret(MACHINE_A);
            let partial = codec.encode(r#"{"account_id_key_wrapped_u1":"a2V5"}"#).unwrap();
            std::fs::write(tmp_path(&path), &partial[..10]).unwrap();

            // The store still reads, from the untouched original.
            let (recovered, is_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(recovered, legacy, "an interrupted migration loses nothing");
            assert!(is_legacy, "the original is still the pre-#882 file");

            // And the retry completes, overwriting the stale temp file.
            write_map_at(&path, &codec, &recovered).unwrap();
            let (after, still_legacy) = read_map_at(&path, &codec).unwrap();
            assert_eq!(after, legacy);
            assert!(!still_legacy);
            std::fs::remove_dir_all(&dir).ok();
        }

        /// If no KEK can be derived we must fail the WRITE rather than fall
        /// back to plaintext — and the existing file must survive that failure.
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

            // The real write path, on a host with no derivable KEK.
            let codec = FileCodec::that_cannot_seal();
            let err = write_map_at(&path, &codec, &legacy).unwrap_err();
            assert!(err.to_string().contains("no machine identity"));

            // Untouched, because `write_map_at` encodes before it opens the
            // temp file at all — there is no plaintext fallback to fall into.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
            assert!(!tmp_path(&path).exists());
            std::fs::remove_dir_all(&dir).ok();
        }

        /// Two writes of identical content must not produce identical bytes —
        /// fresh salt AND fresh nonce every time.
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

        /// A plain READ of a pre-#882 file must both return the key and leave
        /// the file encrypted behind it — an upgrader who only ever unlocks
        /// must not keep their keys in the clear.
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

        /// If the migration write fails, the READ must still succeed — losing
        /// access to keys is strictly worse than leaving the plaintext one more
        /// session.
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

        // ── #950 ────────────────────────────────────────────────────────────

        /// A temp dir plus the two store paths inside it.
        fn migration_dir(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
            let dir = std::env::temp_dir()
                .join(format!("pollis-ks-{tag}-{}", ulid::Ulid::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let new = dir.join("keystore.pks");
            let old = dir.join("dev-keystore.json");
            (dir, new, old)
        }

        /// #950(a) — the rename, done as write-new → verify-readable → unlink.
        /// The old name goes away and not one key goes with it.
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

        /// The ordering property. If the new file cannot be written, the old one
        /// — which is the only copy of an unrecoverable identity key — must
        /// still be there, still readable, and still the store in use.
        #[test]
        fn a_rename_that_cannot_complete_keeps_the_old_file() {
            let (dir, new, old) = migration_dir("rename-fail");
            let codec = FileCodec::with_secret(MACHINE_A);
            let keys = map_of(&[("account_id_key_wrapped_u1", "a2V5")]);
            write_map_at(&old, &codec, &keys).unwrap();
            let before = std::fs::read(&old).unwrap();

            // Reads the old file perfectly, cannot write the new one — so the
            // rename fails at step 2, with the old file already read and the
            // delete one step away. `that_cannot_seal()` would NOT do: it also
            // carries the wrong machine secret, so it fails at step 1 and this
            // test would silently duplicate the wrong-machine case above.
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

        /// An undecryptable old file (wrong machine) must not be migrated,
        /// deleted, or replaced by an empty new one — the caller sees the
        /// ordinary wrong-machine error against the ordinary path.
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

        /// The interrupted window: new file written and verified, crash before
        /// the unlink. Both files exist. The next start must take the new one
        /// and finish the job, not resurrect the old one.
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

        /// #950(c) — a store that decrypts but does not parse is LEFT ALONE.
        ///
        /// The old behaviour renamed it to `dev-keystore.bad-{ts}.json`, which
        /// looks like a backup and functions as a wipe: the very next operation
        /// finds no store, treats the device as new, and the first write
        /// establishes an empty one. This asserts both halves of the fix — the
        /// file stays, and the failure is stable rather than self-clearing.
        #[test]
        fn a_corrupt_store_is_never_moved_aside_or_started_over() {
            let (dir, path, _) = migration_dir("corrupt");
            let codec = FileCodec::with_secret(MACHINE_A);

            // Valid ciphertext, valid UTF-8 inside, not a JSON object.
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

            // The load-bearing half: the NEXT operation must still refuse.
            // Under the old behaviour the file was gone by now, so this write
            // would have quietly created a brand-new store holding only `k` —
            // every other user's keys on this device silently discarded.
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

        /// #950(b) — the namespace rule that decides whether a box which just
        /// grew a keychain stays on the file. Driven over the pure predicate so
        /// all four namespace shapes are covered in one process.
        #[test]
        fn namespace_matching_is_exact_in_every_shape() {
            use super::super::key_is_in_namespace as in_ns;

            // Release, no explicit data dir: the unprefixed namespace.
            assert!(in_ns("", "account_id_key_wrapped_u1"));
            assert!(
                !in_ns("", "dev2:account_id_key_wrapped_u1"),
                "a second instance's key must not pin the default namespace"
            );

            // Debug build.
            assert!(in_ns("DEV:", "DEV:db_key_u1"));
            assert!(!in_ns("DEV:", "db_key_u1"));
            assert!(!in_ns("DEV:", "dev2:DEV:db_key_u1"));

            // Explicit data dir, release and debug.
            assert!(in_ns("dev2:", "dev2:db_key_u1"));
            assert!(!in_ns("dev2:", "dev3:db_key_u1"));
            assert!(!in_ns("dev2:", "db_key_u1"));
            assert!(in_ns("dev2:DEV:", "dev2:DEV:db_key_u1"));
            assert!(!in_ns("dev2:DEV:", "dev3:DEV:db_key_u1"));

            // And the rule agrees with the generator it mirrors, whatever
            // namespace this test process happens to be running in.
            assert!(super::super::is_current_namespace(&super::super::namespaced(
                "account_id_key_wrapped_u1"
            )));
            assert!(!super::super::is_current_namespace(&format!(
                "dev2:{}",
                super::super::namespaced("account_id_key_wrapped_u1")
            )));
        }

        /// The backend-selection input itself: a file holding this namespace's
        /// keys reports true; one holding only a sibling instance's reports
        /// false, so a second dev instance never pins the first's backend.
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

        /// The derivation is a pure function of (secret, salt) — same inputs
        /// give the same key, either input changing gives a different one.
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

// ── OS keychain backend ──────────────────────────────────────────────────────
//
// Requires the `os-keystore` feature (on by default). Compiled independently of
// `debug_assertions` since #882 — which backend a build USES is now decided at
// runtime by `backend::kind()`, so this module being present no longer implies
// it is selected.

#[cfg(feature = "os-keystore")]
mod keychain_backend {
    use super::namespaced;
    use crate::error::{Error, Result};
    use keyring::Entry;

    const SERVICE: &str = "pollis";

    /// Read-only probe for "is there a usable credential store on this host".
    ///
    /// Reading an entry that does not exist is the cheapest question that still
    /// exercises the whole platform path: on Linux it round-trips to the
    /// secret-service over D-Bus, which is exactly what is missing on a server
    /// over SSH. No write, and no prompt on any platform.
    ///
    /// `NoEntry` is the healthy answer. Anything else — `PlatformFailure`
    /// (D-Bus refused / no service), `NoStorageAccess` (store locked) — means
    /// this process cannot rely on the keychain.
    pub fn available() -> bool {
        match Entry::new(SERVICE, &namespaced("backend-probe")).map(|e| e.get_password()) {
            Ok(Ok(_)) | Ok(Err(keyring::Error::NoEntry)) => true,
            Ok(Err(e)) => {
                eprintln!("[keystore] OS keychain unavailable ({e})");
                false
            }
            Err(e) => {
                eprintln!("[keystore] OS keychain unavailable ({e})");
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

// ── Backend selection ────────────────────────────────────────────────────────

/// Which at-rest backend this process uses.
///
/// There is deliberately **no `Plaintext` variant**. Before #882 the third
/// state existed and was reachable by turning one Cargo feature off; encoding
/// the two acceptable outcomes as the only two representable ones is what stops
/// that from being a security cliff again (`CLAUDE.md` — invalid states
/// unrepresentable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// The OS keychain guards the key: macOS Keychain, Windows Credential
    /// Manager, Linux Secret Service. Preferred wherever it exists.
    OsKeychain,
    /// A file whose bytes are AES-256-GCM ciphertext under a machine-bound KEK
    /// (desktop) or an OS-held key (mobile). Used where no keychain exists.
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
/// A build with this `false` can still only reach `EncryptedFile`, never
/// plaintext — but it can never reach the keychain either, which is what #879
/// discovered the hard way about the shipped CLI. `pollis-tui`'s test suite
/// asserts this is `true` so the feature cannot be dropped again in silence.
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
    /// Freezing matters: a keychain that answers on one call and errors on the
    /// next (a D-Bus hiccup — the failure class behind #184) must not move this
    /// process's writes to a different store mid-session. Half the slots in the
    /// keychain and half in the file is the one outcome worse than either.
    fn detect() -> BackendKind {
        // Mobile has no keyring path — the file IS the store, sealed by the
        // Android Keystore / iOS Keychain.
        if cfg!(any(target_os = "android", target_os = "ios")) {
            return BackendKind::EncryptedFile;
        }
        // Debug builds keep using the file so the dev loop never triggers an OS
        // credential prompt. Since #882 that file is encrypted like any other.
        if cfg!(debug_assertions) {
            return BackendKind::EncryptedFile;
        }
        keychain_or_file()
    }

    /// The #882 runtime fallback: the SAME shipped binary uses the keychain on
    /// a desktop and the encrypted file on a headless server, instead of the
    /// build-time coin-flip that forced #879's choice between "plaintext keys"
    /// and "the CLI does not start".
    #[cfg(feature = "os-keystore")]
    fn keychain_or_file() -> BackendKind {
        if !super::keychain_backend::available() {
            eprintln!(
                "[keystore] no usable OS keychain on this host; \
                 falling back to the machine-bound encrypted file store"
            );
            return BackendKind::EncryptedFile;
        }

        // A keychain answers. That does NOT settle it (#950).
        //
        // A headless box that later grows a Secret Service — someone installs a
        // desktop session, or a container image starts shipping gnome-keyring —
        // would otherwise flip backends on the next launch, find the keychain
        // empty, and present the user with PIN-setup or an OTP prompt as if the
        // device had never been enrolled. Nothing is destroyed and re-enrolling
        // heals it, but "your messenger forgot you because a package landed" is
        // not an acceptable way to learn that.
        //
        // So the rule is: whoever already holds this namespace's keys keeps
        // serving them. Only a namespace with no file-side state is free to
        // adopt the keychain.
        //
        // Three consequences worth stating rather than discovering:
        //
        //   * It is one-way in practice. Once a device is on the file it stays
        //     there until that state is deliberately removed (`wipe_local_data`,
        //     or signing out and deleting the data dir), at which point the next
        //     start picks the keychain. That is the correct direction for the
        //     trap to point: never silently abandon keys, always re-evaluate
        //     from clean.
        //   * If BOTH stores hold keys — a box that ran on the keychain, lost it,
        //     re-enrolled onto the file, then got it back — the file wins, and it
        //     should: it holds the more recent enrolment. The keychain's entries
        //     are stale and inert.
        //   * The check is namespace-scoped, so a second dev instance's keys in
        //     the shared file never pin the first instance's backend.
        if super::file_backend::holds_current_namespace_keys() {
            eprintln!(
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

/// Which backend this process settled on. Diagnostic only — the choice is made
/// on first use and never changes for the life of the process.
pub async fn backend_kind() -> BackendKind {
    backend::kind().await
}

// ── Trait abstraction (production + in-memory test impls) ────────────────────

/// Abstraction over secret storage. Production uses [`OsKeystore`], which
/// wraps whichever backend [`backend_kind`] selected: the OS keychain where
/// one exists, otherwise the machine-bound encrypted file under the data dir.
/// Tests use [`InMemoryKeystore`] so every [`TestClient`] gets its own isolated
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

/// Production keystore. A thin delegation to the `backend` dispatcher, which
/// picks the OS keychain or the machine-bound encrypted file once per process.
/// Neither branch can write plaintext — see [`BackendKind`].
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

/// The exact name a per-user slot resolves to for THIS process — the
/// [`Keystore::store_for_user`] key composition followed by the instance
/// namespacing.
///
/// Exposed so `tests/keystore_at_rest.rs` can seed a pre-migration store the
/// shipped `OsKeystore` will actually find, without reaching into the module's
/// internals or hand-copying the naming rule (which is precisely the kind of
/// copy that drifts).
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

    // ── Backend selection guards (#882) ─────────────────────────────────────

    /// GUARD — the backend this process picks is always one that encrypts.
    ///
    /// The `Plaintext` third state that existed before #882 is gone from the
    /// type, so this is really a statement about the enum: adding a plaintext
    /// variant back would have to break this test to compile past it.
    #[tokio::test]
    async fn guard_selected_backend_is_never_plaintext() {
        let kind = backend_kind().await;
        assert!(
            matches!(kind, BackendKind::OsKeychain | BackendKind::EncryptedFile),
            "the only representable backends must both encrypt at rest, got {kind}"
        );
    }

    /// The choice is frozen for the process — a keychain that flakes mid-session
    /// must not split this process's slots across two stores (#184's failure
    /// class).
    #[tokio::test]
    async fn guard_backend_choice_is_stable_within_a_process() {
        let first = backend_kind().await;
        for _ in 0..5 {
            assert_eq!(backend_kind().await, first);
        }
    }

    /// GUARD — a debug build must never reach for the OS keychain; that is what
    /// keeps credential prompts out of the dev loop.
    ///
    /// Only discriminating when `os-keystore` is compiled in (otherwise the
    /// file backend is the only branch there is), which is the case for CI's
    /// `cargo test -p pollis-core`. Gated on `debug_assertions` because
    /// `cargo test --release` legitimately takes the other branch.
    #[cfg(debug_assertions)]
    #[tokio::test]
    async fn debug_builds_use_the_file_backend() {
        assert_eq!(backend_kind().await, BackendKind::EncryptedFile);
    }

    // ── kek_envelope (the iOS + desktop at-rest cipher) — host-testable ─────

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
        // Pins the on-disk layout shared with android_kek: iv(12) || ct || tag(16).
        let key = [1u8; 32];
        let pt = b"0123456789";
        let blob = kek_envelope::seal_with(&key, pt).unwrap();
        assert_eq!(blob.len(), 12 + pt.len() + 16);
        // Fresh random IV per seal — two seals of the same plaintext differ.
        let blob2 = kek_envelope::seal_with(&key, pt).unwrap();
        assert_ne!(blob[..12], blob2[..12], "IV must be random per seal");
    }

    #[test]
    fn kek_envelope_rejects_wrong_key_and_tamper() {
        let key = [2u8; 32];
        let blob = kek_envelope::seal_with(&key, b"secret").unwrap();
        // Wrong key.
        assert!(kek_envelope::unseal_with(&[3u8; 32], &blob).is_err());
        // Bit-flip in the ciphertext body → GCM auth failure.
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(kek_envelope::unseal_with(&key, &tampered).is_err());
        // Truncated below the IV → structured error, not a panic.
        assert!(kek_envelope::unseal_with(&key, &blob[..8]).is_err());
    }
}
