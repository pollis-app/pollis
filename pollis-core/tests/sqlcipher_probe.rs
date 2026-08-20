//! Throwaway diagnostic branch (#991) — NOT for merge.
//!
//! Question: at v1.9.9 (before #988 removed libsql), was the sqlite3 this crate
//! linked SQLCipher, or libsql-ffi's plain amalgamation? `PRAGMA cipher_version`
//! answers only in SQLCipher.
//!
//! Test PASSES  => SQLCipher was linked before #988.
//! Test FAILS   => it was not, and `PRAGMA key` was a silently-ignored pragma.
#[test]
fn cipher_version_is_answered() {
    let conn = rusqlite::Connection::open_in_memory().expect("open");
    let version: Option<String> = conn
        .query_row("PRAGMA cipher_version", [], |row| row.get(0))
        .ok();
    println!("PROBE cipher_version = {version:?}");
    assert!(
        version.as_deref().map(str::trim).is_some_and(|v| !v.is_empty()),
        "no cipher_version: the linked sqlite3 is NOT SQLCipher"
    );
}
