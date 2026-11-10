//! Compiles the SQLCipher amalgamation from `sqlcipher-src`.
//!
//! The source is the same file `libsqlite3-sys`'s `bundled-sqlcipher` builds on
//! every other platform (today the workspace's patched `sqlcipher-src`, SQLCipher
//! 4.20.0 on SQLite 3.54.1; see patches/sqlcipher-src/Cargo.toml). Flags mirror
//! `libsqlite3-sys` 0.39.1's `build_bundled` list for `bundled-sqlcipher`.

fn main() {
    let source = sqlcipher_src::source_dir().join(sqlcipher_src::SOURCE_FILE);
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=include/tomcrypt.h");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut cfg = cc::Build::new();
    // Only our `include/` goes on the path: sqlcipher-src also ships the real
    // libtomcrypt headers, and its `tomcrypt.h` must not shadow ours.
    cfg.file(&source)
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
        .define("SQLITE_ENABLE_RTREE", None)
        .define("SQLITE_ENABLE_STAT4", None)
        .define("SQLITE_SOUNDEX", None)
        .define("SQLITE_THREADSAFE", "1")
        .define("SQLITE_USE_URI", None)
        .define("HAVE_USLEEP", "1")
        .define("HAVE_ISNAN", None)
        .define("HAVE_STDINT_H", "1")
        // Temp tables and the rollback journal must never land unencrypted.
        .define("SQLITE_HAS_CODEC", None)
        .define("SQLITE_TEMP_STORE", "2")
        // Current SQLCipher refuses to compile without its init/shutdown hooks.
        .define("SQLITE_EXTRA_INIT", "sqlcipher_extra_init")
        .define("SQLITE_EXTRA_SHUTDOWN", "sqlcipher_extra_shutdown")
        // Our RustCrypto provider instead of OpenSSL.
        .define("SQLCIPHER_CRYPTO_LIBTOMCRYPT", None)
        .warnings(false);

    if target_os != "windows" {
        cfg.define("HAVE_LOCALTIME_R", None);
    }

    cfg.compile("sqlcipher");

    if target_os == "linux" || target_os == "android" {
        println!("cargo:rustc-link-lib=dylib=m");
        println!("cargo:rustc-link-lib=dylib=dl");
        println!("cargo:rustc-link-lib=dylib=pthread");
    }
}
