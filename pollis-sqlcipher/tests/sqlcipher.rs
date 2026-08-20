//! Drives a real database through the vendored SQLCipher and the RustCrypto
//! provider behind it, then reads the resulting file back as raw bytes.
//!
//! `PRAGMA cipher_version` answering proves only that a SQLCipher build is
//! present — it says nothing about whether the pages that reached the disk are
//! ciphertext. That is what this file settles, and it settles it on every
//! platform: the amalgamation is compiled everywhere (see `build.rs`), so a
//! provider mistake fails the ordinary Linux gate rather than surviving until
//! the Windows job runs.
//!
//! The FFI here is hand-declared rather than routed through `rusqlite` on
//! purpose. This crate must not depend on `libsqlite3-sys`: a second sqlite3 in
//! the same test binary is exactly the "which library won the `sqlite3_*`
//! symbols" ambiguity that hid the Windows plaintext bug for a year.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;

// Not for its Rust API — it has none worth calling here. Naming the crate is
// what puts its rlib on the link line, and with it the `sqlcipher.lib` /
// `libsqlcipher.a` its build script produced plus the `#[no_mangle]` crypto
// provider the amalgamation calls back into.
use pollis_sqlcipher as _;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;

extern "C" {
    fn sqlite3_open(filename: *const c_char, db: *mut *mut c_void) -> c_int;
    fn sqlite3_close(db: *mut c_void) -> c_int;
    fn sqlite3_exec(
        db: *mut c_void,
        sql: *const c_char,
        callback: *const c_void,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut c_void,
        sql: *const c_char,
        n_byte: c_int,
        stmt: *mut *mut c_void,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut c_void) -> c_int;
    fn sqlite3_column_text(stmt: *mut c_void, col: c_int) -> *const u8;
    fn sqlite3_finalize(stmt: *mut c_void) -> c_int;
    fn sqlite3_libversion() -> *const c_char;
}

/// A `sqlite3*` that closes itself, so a failing assertion cannot leave the
/// file locked for the next step of the test.
struct Db(*mut c_void);

impl Drop for Db {
    fn drop(&mut self) {
        unsafe {
            sqlite3_close(self.0);
        }
    }
}

impl Db {
    fn open(path: &Path) -> Db {
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        let mut handle: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { sqlite3_open(c_path.as_ptr(), &mut handle) };
        assert_eq!(rc, SQLITE_OK, "sqlite3_open({path:?})");
        Db(handle)
    }

    fn exec(&self, sql: &str) -> c_int {
        let c_sql = CString::new(sql).unwrap();
        unsafe {
            sqlite3_exec(
                self.0,
                c_sql.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    }

    fn must_exec(&self, sql: &str) {
        assert_eq!(self.exec(sql), SQLITE_OK, "exec: {sql}");
    }

    /// First column of the first row, or `None` when the statement yields no
    /// rows — which is how SQLCipher answers a pragma it does not know.
    fn query_one(&self, sql: &str) -> Option<String> {
        let c_sql = CString::new(sql).unwrap();
        let mut stmt: *mut c_void = std::ptr::null_mut();
        unsafe {
            let rc = sqlite3_prepare_v2(
                self.0,
                c_sql.as_ptr(),
                -1,
                &mut stmt,
                std::ptr::null_mut(),
            );
            if rc != SQLITE_OK || stmt.is_null() {
                return None;
            }
            let value = if sqlite3_step(stmt) == SQLITE_ROW {
                let text = sqlite3_column_text(stmt, 0);
                if text.is_null() {
                    None
                } else {
                    Some(CStr::from_ptr(text as *const c_char).to_string_lossy().into_owned())
                }
            } else {
                None
            };
            sqlite3_finalize(stmt);
            value
        }
    }

    /// The key SQLCipher wants before any other statement, in the same
    /// raw-key form `pollis-core` uses so this exercises that path too.
    fn key(&self, key: &[u8]) {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        self.must_exec(&format!("PRAGMA key = \"x'{hex}'\""));
    }
}

const KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdef";
const WRONG_KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdeF";
const MARKER: &str = "sqlcipher-plaintext-canary-a7f3d1";

fn scratch(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(name).tempdir().expect("tempdir")
}

/// The library that answered is SQLCipher, and it is the provider this crate
/// built — not some other sqlite3 that drifted into the link.
#[test]
fn the_linked_library_is_our_sqlcipher() {
    let dir = scratch("cipher-version");
    let db = Db::open(&dir.path().join("probe.db"));
    // `cipher_provider*` only answers once a codec has been attached, so key the
    // connection first.
    db.key(KEY);
    db.must_exec("CREATE TABLE t (x)");

    let version = db.query_one("PRAGMA cipher_version");
    assert!(
        version.as_deref().map(str::trim).is_some_and(|v| !v.is_empty()),
        "`PRAGMA cipher_version` answered {version:?}: the linked sqlite3 is not SQLCipher"
    );

    assert_eq!(
        db.query_one("PRAGMA cipher_provider_version").as_deref(),
        Some("pollis-rustcrypto-1"),
        "the crypto provider is not the one in src/abi.rs (cipher_provider said {:?})",
        db.query_one("PRAGMA cipher_provider")
    );

    let sqlite = unsafe { CStr::from_ptr(sqlite3_libversion()) }.to_string_lossy().into_owned();
    assert_eq!(sqlite, "3.46.1", "vendor/sqlite3.c is not the version PROVENANCE.md records");
}

/// The whole point. Write a distinctive string into a keyed database, close it,
/// and look at the bytes that are actually on disk.
#[test]
fn a_keyed_database_is_ciphertext_on_disk() {
    let dir = scratch("encrypted-at-rest");
    let path = dir.path().join("encrypted.db");

    {
        let db = Db::open(&path);
        db.key(KEY);
        db.must_exec("CREATE TABLE message (id INTEGER PRIMARY KEY, body TEXT)");
        db.must_exec(&format!("INSERT INTO message (body) VALUES ('{MARKER}')"));
    }

    let bytes = std::fs::read(&path).expect("read database file");
    assert!(!bytes.is_empty(), "the database file is empty");

    assert!(
        !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
        "the plaintext marker is present in {} bytes of database file — it is NOT encrypted",
        bytes.len()
    );

    // SQLCipher encrypts the file header too, so a readable magic string is an
    // immediate tell that the codec never engaged.
    assert_ne!(
        &bytes[..16],
        b"SQLite format 3\0",
        "the file begins with the plaintext SQLite header — `PRAGMA key` did nothing"
    );

    // ...and the data is still there when the key is supplied.
    let db = Db::open(&path);
    db.key(KEY);
    assert_eq!(db.query_one("SELECT body FROM message").as_deref(), Some(MARKER));
}

/// Reading it back must require the key. A build where the wrong key — or no
/// key — still returns rows is a build that is not encrypting.
#[test]
fn the_wrong_key_cannot_read_the_database() {
    let dir = scratch("wrong-key");
    let path = dir.path().join("encrypted.db");

    {
        let db = Db::open(&path);
        db.key(KEY);
        db.must_exec("CREATE TABLE message (id INTEGER PRIMARY KEY, body TEXT)");
        db.must_exec(&format!("INSERT INTO message (body) VALUES ('{MARKER}')"));
    }

    let no_key = Db::open(&path);
    assert_ne!(
        no_key.exec("SELECT count(*) FROM sqlite_master"),
        SQLITE_OK,
        "an unkeyed connection read the schema"
    );
    assert_eq!(no_key.query_one("SELECT body FROM message"), None);
    drop(no_key);

    let wrong = Db::open(&path);
    wrong.key(WRONG_KEY);
    assert_ne!(
        wrong.exec("SELECT count(*) FROM sqlite_master"),
        SQLITE_OK,
        "a connection with the wrong key read the schema"
    );
    assert_eq!(wrong.query_one("SELECT body FROM message"), None);
}

/// SQLCipher 4's defaults are what make the file above ciphertext; a build that
/// silently negotiated something weaker would still pass the tests above.
#[test]
fn the_cipher_parameters_are_sqlcipher_4_defaults() {
    let dir = scratch("cipher-params");
    let db = Db::open(&dir.path().join("params.db"));
    db.key(KEY);
    db.must_exec("CREATE TABLE t (x)");

    assert_eq!(db.query_one("PRAGMA cipher").as_deref(), Some("aes-256-cbc"));
    assert_eq!(db.query_one("PRAGMA cipher_hmac_algorithm").as_deref(), Some("HMAC_SHA512"));
    assert_eq!(
        db.query_one("PRAGMA cipher_kdf_algorithm").as_deref(),
        Some("PBKDF2_HMAC_SHA512")
    );
    assert_eq!(db.query_one("PRAGMA kdf_iter").as_deref(), Some("256000"));
}
