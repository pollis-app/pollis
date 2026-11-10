# sqlcipher-src

[![CI](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/ci.yml/badge.svg)](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/ci.yml)
[![CodeQL](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/codeql.yml/badge.svg)](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/codeql.yml)
[![ClusterFuzzLite](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/cflite_build.yml/badge.svg)](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/cflite_build.yml)
[![codecov](https://codecov.io/gh/LucaCappelletti94/sqlcipher-src/graph/badge.svg?token=iyDt4aR7wE)](https://codecov.io/gh/LucaCappelletti94/sqlcipher-src)
[![SQLCipher release](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/sqlcipher-release.yml/badge.svg)](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/sqlcipher-release.yml)
[![SQLCipher previews](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/sqlcipher-previews.yml/badge.svg)](https://github.com/LucaCappelletti94/sqlcipher-src/actions/workflows/sqlcipher-previews.yml)
[![crates.io](https://img.shields.io/crates/v/sqlcipher-src.svg)](https://crates.io/crates/sqlcipher-src)
[![docs.rs](https://docs.rs/sqlcipher-src/badge.svg)](https://docs.rs/sqlcipher-src)
[![license](https://img.shields.io/badge/license-MIT%20AND%20BSD--3--Clause%20AND%20blessing%20AND%20WTFPL-blue.svg)](https://github.com/LucaCappelletti94/sqlcipher-src/blob/main/Cargo.toml)

The [SQLCipher](https://github.com/sqlcipher/sqlcipher) amalgamation and the [libtomcrypt](https://github.com/libtom/libtomcrypt) crypto provider as C source for `-sys` crates.

Native builds compile `SOURCE_FILE` with their own crypto provider and flags:

```rust
let dir = sqlcipher_src::source_dir();
assert!(dir.join(sqlcipher_src::SOURCE_FILE).is_file());
assert!(dir.join(sqlcipher_src::HEADER_FILE).is_file());
```

Builds without a system crypto library compile `SOURCE_FILE` with `-DSQLITE_HAS_CODEC -DSQLCIPHER_CRYPTO_LIBTOMCRYPT` and each file in `LIBTOMCRYPT_SOURCES` as its own translation unit with `-DLTC_SOURCE`, all with `LIBTOMCRYPT_INCLUDE_DIR` on the include path and one shared set of `LTC_*` switches:

```rust
let dir = sqlcipher_src::source_dir();
assert!(dir.join(sqlcipher_src::LIBTOMCRYPT_INCLUDE_DIR).join("tomcrypt.h").is_file());
for source in sqlcipher_src::LIBTOMCRYPT_SOURCES {
    assert!(dir.join(source).is_file());
}
```

The `sqlcipher` feature of [`sqlite-wasm-rs`](https://github.com/Spxg/sqlite-wasm-rs) builds this way for `wasm32-unknown-unknown`.

The sources are generated from signed SQLCipher and libtomcrypt releases by `upgrade.sh` and ship as released. CI runs SQLCipher's and `rusqlite`'s SQLCipher tests on them natively, and checks through `sqlite-wasm-rs` that files written natively and in Node, Chrome and Firefox open on both sides.

The same checks run on SQLCipher's newest signed pre-release, and daily on the heads of its `prerelease` and `beta` branches. Nothing from those lines ships, so they report under the SQLCipher previews badge and never hold back the CI badge or a release.

[ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) fuzzes SQLCipher as `sqlite-wasm-rs` compiles it, under `AddressSanitizer`, `UndefinedBehaviorSanitizer` and `MemorySanitizer`, on the shipped release, SQLCipher's newest signed pre-release and its `prerelease` branch. A script target drives keys, rekeys, codec settings, attach, backup, export, page damage and injected I/O and allocation faults. It checks that a tampered page never reads back as valid, that no plaintext reaches a keyed file, that IVs and salts never repeat, that operations meant to keep content keep it, and that the rows match what plain SQLite and the OpenSSL provider produce. Two file-image targets hand SQLCipher hostile database files, one without the key and one with it.

The crate is MIT, SQLCipher is BSD-3-Clause, SQLite is public domain, and libtomcrypt is public domain or WTFPL.
