//! Compiles the vendored SQLCipher amalgamation.
//!
//! The flag list below is `libsqlite3-sys` 0.35.0's `build_bundled` list for a
//! Windows target, verbatim, plus the three flags that turn SQLCipher on and
//! point it at our crypto provider. Keeping it identical matters: the Rust
//! bindings `libsqlite3-sys` hands `rusqlite` come from its own
//! `sqlcipher/bindgen_bundled_version.rs`, which was generated against this
//! exact amalgamation with these exact flags, so a divergence here is a
//! divergence between the declarations and the library.
//!
//! It is built on EVERY target, not just Windows. Only Windows links it into
//! the application — `pollis-core` depends on this crate under `cfg(windows)`,
//! and Linux/macOS/mobile keep `bundled-sqlcipher`'s own copy — but building it
//! everywhere is what lets `tests/sqlcipher.rs` prove the crypto provider
//! actually encrypts a database on the ordinary Linux gate, instead of that
//! evidence existing only inside a 40-minute Windows job.

fn main() {
    println!("cargo:rerun-if-changed=vendor/sqlite3.c");
    println!("cargo:rerun-if-changed=include/tomcrypt.h");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut cfg = cc::Build::new();
    cfg.file("vendor/sqlite3.c")
        // Where `#include <tomcrypt.h>` resolves. This directory holds exactly
        // one header and it is ours.
        .include("include")
        .define("SQLITE_CORE", None)
        .define("SQLITE_DEFAULT_FOREIGN_KEYS", "1")
        .define("SQLITE_ENABLE_API_ARMOR", None)
        .define("SQLITE_ENABLE_COLUMN_METADATA", None)
        .define("SQLITE_ENABLE_DBSTAT_VTAB", None)
        .define("SQLITE_ENABLE_FTS3", None)
        .define("SQLITE_ENABLE_FTS3_PARENTHESIS", None)
        .define("SQLITE_ENABLE_FTS5", None)
        .define("SQLITE_ENABLE_JSON1", None)
        .define("SQLITE_ENABLE_LOAD_EXTENSION", "1")
        .define("SQLITE_ENABLE_MEMORY_MANAGEMENT", None)
        .define("SQLITE_ENABLE_RTREE", None)
        .define("SQLITE_ENABLE_STAT4", None)
        .define("SQLITE_SOUNDEX", None)
        .define("SQLITE_THREADSAFE", "1")
        .define("SQLITE_USE_URI", None)
        .define("HAVE_USLEEP", "1")
        .define("HAVE_ISNAN", None)
        // The codec itself, and the temp-store setting SQLCipher requires so
        // that temporary tables and the rollback journal never land unencrypted
        // on disk.
        .define("SQLITE_HAS_CODEC", None)
        .define("SQLITE_TEMP_STORE", "2")
        // Selects `crypto_libtomcrypt.c` as the provider. Without it the
        // amalgamation defaults to `SQLCIPHER_CRYPTO_OPENSSL` and wants
        // <openssl/evp.h>, which is the whole problem.
        .define("SQLCIPHER_CRYPTO_LIBTOMCRYPT", None)
        .warnings(false);

    // `libsqlite3-sys` adds this on every non-Windows target; keeping parity
    // matters most on the platforms where this build exists only to be tested.
    if target_os != "windows" {
        cfg.define("HAVE_LOCALTIME_R", None);
    }

    cfg.compile("sqlcipher");

    // SQLite's own system dependencies. Windows needs none of these, and macOS
    // has them all in libSystem, which rustc already links.
    if target_os == "linux" || target_os == "android" {
        println!("cargo:rustc-link-lib=dylib=m");
        println!("cargo:rustc-link-lib=dylib=dl");
        println!("cargo:rustc-link-lib=dylib=pthread");
    }
}
