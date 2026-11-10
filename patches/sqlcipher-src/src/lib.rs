#![doc = include_str!("../README.md")]

use std::path::Path;

/// SQLCipher release the vendored amalgamation is generated from.
pub const SQLCIPHER_VERSION: &str = "4.20.0";

/// SQLite release that SQLCipher release is based on.
pub const SQLITE_VERSION: &str = "3.54.1";

/// libtomcrypt release providing the ciphers, hashes and random generator.
pub const LIBTOMCRYPT_VERSION: &str = "1.18.2";

/// SQLCipher amalgamation inside [`source_dir`], with the crypto provider left to the build.
pub const SOURCE_FILE: &str = "sqlcipher.c";

/// Public SQLCipher header inside [`source_dir`].
pub const HEADER_FILE: &str = "sqlite3.h";

/// libtomcrypt's header directory inside [`source_dir`], which SQLCipher's `<tomcrypt.h>` and every file in
/// [`LIBTOMCRYPT_SOURCES`] need on the include path.
pub const LIBTOMCRYPT_INCLUDE_DIR: &str = "libtomcrypt/headers";

/// libtomcrypt files inside [`source_dir`], each compiled as its own translation unit.
///
/// Each needs `LTC_SOURCE` defined and the build's `LTC_*` configuration, as upstream builds them. The tables they
/// `#include` are left out.
pub const LIBTOMCRYPT_SOURCES: &[&str] = &include!("libtomcrypt_sources.rs");

/// Directory holding the sources, at the path this crate was compiled from.
#[must_use]
pub fn source_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/sqlcipher"))
}
