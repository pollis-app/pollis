//! SQLCipher for Windows, with a crypto provider that is not OpenSSL.
//!
//! # Why this crate exists
//!
//! `rusqlite`'s `bundled-sqlcipher*` features build SQLCipher against OpenSSL
//! (or, on Apple platforms, CommonCrypto). On Windows there is no shared system
//! libcrypto, so those features link a **static** OpenSSL — and the MSVC linker
//! resolves a static archive by pulling whole object files. `libwebrtc-sys`
//! already puts a complete BoringSSL in the same image for the voice stack, and
//! the two libraries define the same C symbols, so the second one to be pulled
//! collides: 1,253 fatal `LNK2005` errors (#991, #992). Unlike ELF, where a
//! duplicate definition in a shared object is simply unused, MSVC makes it an
//! error, and there is no feature combination that avoids it.
//!
//! Compiling SQLCipher against OpenSSL headers and letting it *resolve* to the
//! BoringSSL already present is not a fix either, however tempting: BoringSSL
//! deliberately changed the signature of `PKCS5_PBKDF2_HMAC` (and others) from
//! `int` lengths to `size_t`. The call would link and then pass 32-bit
//! arguments into 64-bit parameters — silent, undefined, and exactly the class
//! of "green build, broken crypto" this whole issue already was once.
//!
//! # What this crate does instead
//!
//! `vendor/sqlite3.c` is the stock SQLCipher amalgamation, compiled here with
//! `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT`. That selects the one upstream crypto
//! provider whose entire dependency surface is a single header we are free to
//! supply ourselves — `include/tomcrypt.h`, ~20 functions — and [`abi`] backs
//! every one of them with the RustCrypto crates pollis already depends on:
//! `aes`, `sha1`, `sha2`, `hmac`, `pbkdf2`. No OpenSSL, no BoringSSL, no
//! libtomcrypt, and nothing new in the dependency graph.
//!
//! The ciphertext is unmodified SQLCipher 4: AES-256-CBC pages, per-page
//! HMAC-SHA512, PBKDF2-HMAC-SHA512 key derivation. `PRAGMA cipher_provider`
//! answers `libtomcrypt` because the provider *shape* is upstream's and we chose
//! not to patch the amalgamation; `PRAGMA cipher_provider_version` answers
//! `pollis-rustcrypto-1` and is the honest one.
//!
//! # How it reaches rusqlite
//!
//! On Windows, `pollis-core` takes `rusqlite` with the plain `sqlcipher` feature
//! (link against an external SQLCipher) rather than `bundled-sqlcipher`, so
//! `libsqlite3-sys` emits `-l sqlcipher` and compiles no C of its own. The build
//! script here produces `sqlcipher.lib` and puts its directory on the link
//! search path, so that `-l` resolves to this crate's amalgamation and to
//! nothing else. Exactly one sqlite3 ends up in the image — the checked-in
//! `sqlcipher_is_the_sqlite_we_actually_linked` test in
//! `pollis-core/src/db/local.rs` is what holds that true.
//!
//! On every other target this crate compiles no C and links nothing:
//! Linux/macOS/mobile keep `bundled-sqlcipher` exactly as before. The Rust in
//! [`abi`] is still built and still unit-tested there, which is the point — the
//! crypto is verified by known-answer tests on every platform's CI, not only on
//! the one that ships it.

pub mod abi;
