//! Android-only TLS bootstrap.
//!
//! `rustls-native-certs` reads `/etc/ssl/certs/*.pem` on Android, which does not
//! exist there, and panics on the first connection. It honours `SSL_CERT_FILE`,
//! so we ship the Mozilla CA bundle (`assets/cacert.pem`, from
//! https://curl.se/ca/cacert.pem), write it to the sandbox at init and point
//! `SSL_CERT_FILE` at it. No current dependency needs it since #987 dropped
//! `libsql`; it stays as a safety net for any future caller.

#![cfg(target_os = "android")]

use std::path::{Path, PathBuf};

const CA_BUNDLE: &[u8] = include_bytes!("../assets/cacert.pem");

pub fn install(data_dir: &Path) -> std::io::Result<()> {
    crate::private_fs::create_dir_all(data_dir)?;
    let bundle_path = data_dir.join("cacert.pem");
    let needs_write = match std::fs::metadata(&bundle_path) {
        Ok(m) => m.len() as usize != CA_BUNDLE.len(),
        Err(_) => true,
    };
    if needs_write {
        crate::private_fs::write(&bundle_path, CA_BUNDLE)?;
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
