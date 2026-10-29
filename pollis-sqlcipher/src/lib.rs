//! SQLCipher for Windows, with a RustCrypto provider instead of OpenSSL (#992).
//!
//! Static OpenSSL collides with `libwebrtc-sys`'s BoringSSL under MSVC, and
//! resolving to BoringSSL is wrong (its `PKCS5_PBKDF2_HMAC` takes `size_t`
//! lengths). So the stock amalgamation is built with
//! `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT` and [`abi`] implements that ~20-function
//! header over RustCrypto. Ciphertext is unmodified SQLCipher 4.
//! `PRAGMA cipher_provider_version` answers `pollis-rustcrypto-1`. See
//! `.codesight/wiki/database.md` and `docs/security-whitepaper.md` §7.0.
//!
//! Only Windows links this crate into the app (rusqlite's plain `sqlcipher`
//! feature resolves `-l sqlcipher` here). The C is compiled on every target so
//! `tests/sqlcipher.rs` exercises the provider on the Linux gate.

pub mod abi;
