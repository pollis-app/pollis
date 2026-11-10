//! Pollis stand-in for `openmls_sqlite_storage` 0.2.0 (the copy in openmls at
//! the rev the workspace `[patch.crates-io]` pins).
//!
//! Pollis never uses openmls's SQLite storage provider: MLS state lives in our
//! own SQLCipher database through `pollis-core`'s storage provider, and nothing
//! enables openmls's `sqlite-provider` feature. The crate still sits in the
//! lockfile, because cargo resolves every optional dependency, and at that rev it
//! asks for `rusqlite = "0.37"` with plain `bundled`. `libsqlite3-sys` declares
//! `links = "sqlite3"`, so that unused optional edge alone held the workspace on
//! `libsqlite3-sys` 0.35 (SQLite 3.46) and kept us off the releases that fix
//! RUSTSEC-2026-0459.
//!
//! This crate has the upstream name, version and feature names and nothing else.
//! It must never be compiled: if a change ever enables `sqlite-provider`, the
//! build stops here instead of quietly linking a second SQLite beside SQLCipher.
//!
//! Remove this crate and its patch entry when the openmls rev moves to one whose
//! `openmls_sqlite_storage` accepts the `rusqlite` this workspace uses.

compile_error!(
    "openmls_sqlite_storage is stubbed out by patches/openmls-sqlite-storage: \
     Pollis must not enable openmls's `sqlite-provider` feature"
);
