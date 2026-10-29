//! Compiles the vendored SQLCipher amalgamation.
//!
//! Flags mirror `libsqlite3-sys` 0.35.0's Windows `build_bundled` list, which its bindings were generated against.

fn main() {
    println!("cargo:rerun-if-changed=vendor/sqlite3.c");
    println!("cargo:rerun-if-changed=include/tomcrypt.h");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut cfg = cc::Build::new();
    cfg.file("vendor/sqlite3.c")
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
        // Temp tables and the rollback journal must never land unencrypted.
        .define("SQLITE_HAS_CODEC", None)
        .define("SQLITE_TEMP_STORE", "2")
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
