//! Android-only TLS bootstrap.
//!
//! `libsql 0.6` pulls `hyper-rustls 0.25` with its default `native-tokio`
//! feature, which means `rustls-native-certs::load_native_certs()` is the
//! root-of-trust path at connection time. On Android that function uses
//! the unix backend, which reads `/etc/ssl/certs/*.pem` — those files do
//! not exist on Android (Android stores roots as DER blobs in
//! `/system/etc/security/cacerts/` with `c_rehash`-style hashed names).
//! The result is a `TLS error: no valid native root CA certificates
//! found` panic the moment `init_pollis` tries to dial Turso.
//!
//! `rustls-native-certs` *does* respect `SSL_CERT_FILE`, and if set will
//! load roots from that PEM file instead of touching the platform store.
//! So we ship the Mozilla CA bundle (`assets/cacert.pem`, sourced from
//! https://curl.se/ca/cacert.pem) inside the static lib, write it once
//! to the app sandbox at init, and point `SSL_CERT_FILE` there. After
//! that every rustls-native-certs caller in the dependency tree (libsql,
//! hyper-rustls, future deps) gets a working root store.
//!
//! Desktop is untouched: this module compiles to nothing off-Android,
//! and the PEM bytes are only `include_bytes!`'d on Android so the
//! desktop binary doesn't carry the ~190KB blob.

#![cfg(target_os = "android")]

use std::path::{Path, PathBuf};

const CA_BUNDLE: &[u8] = include_bytes!("../assets/cacert.pem");

pub fn install(data_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let bundle_path = data_dir.join("cacert.pem");
    let needs_write = match std::fs::metadata(&bundle_path) {
        Ok(m) => m.len() as usize != CA_BUNDLE.len(),
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&bundle_path, CA_BUNDLE)?;
    }
    set_env("SSL_CERT_FILE", &bundle_path);
    Ok(())
}

fn set_env(key: &str, value: &PathBuf) {
    // SAFETY: init_pollis runs on a single tokio task at process startup,
    // before any other code reads these env vars. set_var is unsound only
    // under concurrent reads/writes from other threads.
    unsafe {
        std::env::set_var(key, value);
    }
}
