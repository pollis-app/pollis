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

    /// Keying with a passphrase rather than raw bytes, which is what puts the
    /// full `kdf_iter`-round PBKDF2 on the page key. `pollis-core` never does
    /// this, but the interop fixtures do, so that the expensive derivation is
    /// compared against another implementation too.
    fn key_passphrase(&self, passphrase: &str) {
        self.must_exec(&format!("PRAGMA key = '{passphrase}'"));
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

/// The tests above fit in a handful of pages. A database large enough to span
/// many of them, written and then re-read across a close, is what actually
/// exercises the provider's CBC chaining, per-page IVs and per-page MACs at
/// volume — a provider that got the IV or the chaining wrong on any page but the
/// first would still pass a single-page round trip.
#[test]
fn a_multi_page_database_round_trips() {
    const ROWS: i64 = 4000;
    let dir = scratch("many-pages");
    let path = dir.path().join("big.db");

    {
        let db = Db::open(&path);
        db.key(KEY);
        db.must_exec("CREATE TABLE message (id INTEGER PRIMARY KEY, body TEXT)");
        db.must_exec("BEGIN");
        for i in 0..ROWS {
            db.must_exec(&format!("INSERT INTO message (id, body) VALUES ({i}, '{MARKER}-{i}')"));
        }
        db.must_exec("COMMIT");
    }

    let bytes = std::fs::read(&path).expect("read database file");
    assert!(bytes.len() > 64 * 1024, "the fixture is only {} bytes — not multi-page", bytes.len());
    assert!(
        !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
        "a row body appears verbatim in a {}-byte database",
        bytes.len()
    );

    let db = Db::open(&path);
    db.key(KEY);
    assert_eq!(db.query_one("SELECT count(*) FROM message").as_deref(), Some("4000"));
    // Both ends and the middle, so a page decrypted with the wrong chaining
    // anywhere in the file shows up.
    for id in [0, ROWS / 2, ROWS - 1] {
        assert_eq!(
            db.query_one(&format!("SELECT body FROM message WHERE id = {id}")).as_deref(),
            Some(format!("{MARKER}-{id}").as_str())
        );
    }
    assert_eq!(db.query_one("PRAGMA integrity_check").as_deref(), Some("ok"));
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

// ── Cross-provider interop ───────────────────────────────────────────────────

/// Keep in step with `db::local::tests` in `pollis-core`, which writes the
/// fixtures these constants describe.
const FIXTURE_PASSPHRASE: &str = "pollis-interop-fixture-passphrase";
const FIXTURE_RAW_KEY: &[u8; 32] = b"pollis-interop-fixture-key-32byt";
const FIXTURE_ROWS: i64 = 64;

fn fixture_body(id: i64) -> String {
    format!("interop-row-{id:03}-{}", "ab".repeat(96))
}

/// Copy a checked-in fixture somewhere writable. SQLCipher wants to journal
/// even for reads, and the fixture is an artifact — a test that mutated it in
/// place would quietly rewrite the thing it is checking against.
fn fixture_copy(dir: &Path, name: &str) -> std::path::PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let dst = dir.join(name);
    std::fs::copy(&src, &dst)
        .unwrap_or_else(|e| panic!("copy fixture {}: {e}", src.display()));
    dst
}

/// The check no round trip can perform: read a database **somebody else
/// encrypted**.
///
/// Every other test in this file writes with this provider and reads with this
/// provider. That cannot distinguish correct SQLCipher from self-consistent
/// nonsense — a provider deriving the wrong key derives the same wrong key on
/// both sides, so the data comes back and the file is still not SQLCipher 4.
/// The mutation that motivated this (#1002) was exactly that shape: a PBKDF2
/// mishandling `dkLen` ≠ `hLen` passed all 14 tests here.
///
/// These fixtures were written by `pollis-core`'s SQLCipher, which on Linux and
/// macOS is compiled against OpenSSL/CommonCrypto rather than `src/abi.rs`. If
/// this provider can read them, the two agree on PBKDF2-HMAC-SHA512 key
/// derivation, AES-256-CBC page encryption and the per-page HMAC — across
/// several pages, so page 1's salt handling is not the only thing exercised.
/// If it cannot, one of them is wrong, and until now nothing would have said so.
#[test]
fn a_database_another_implementation_encrypted_reads_back() {
    let dir = scratch("interop");

    for (name, keying) in [
        ("openssl-passphrase.db", Keying::Passphrase),
        ("openssl-rawkey.db", Keying::Raw),
    ] {
        let path = fixture_copy(dir.path(), name);
        let db = Db::open(&path);
        match keying {
            Keying::Passphrase => db.key_passphrase(FIXTURE_PASSPHRASE),
            Keying::Raw => db.key(FIXTURE_RAW_KEY),
        }

        assert_eq!(
            db.query_one("SELECT count(*) FROM interop").as_deref(),
            Some(FIXTURE_ROWS.to_string().as_str()),
            "{name}: this provider could not read a database OpenSSL's SQLCipher wrote — \
             the two disagree about SQLCipher 4"
        );

        // First row, a row past the first page, and the last one.
        for id in [1, FIXTURE_ROWS / 2, FIXTURE_ROWS] {
            assert_eq!(
                db.query_one(&format!("SELECT body FROM interop WHERE id = {id}")).as_deref(),
                Some(fixture_body(id).as_str()),
                "{name}: row {id} decrypted to the wrong bytes"
            );
        }
    }
}

/// The fixtures have to still be encrypted, or the test above would pass by
/// reading plaintext and prove nothing about the crypto at all.
#[test]
fn the_interop_fixtures_are_themselves_ciphertext() {
    for name in ["openssl-passphrase.db", "openssl-rawkey.db"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        let bytes = std::fs::read(&path).expect("read fixture");
        assert_ne!(
            &bytes[..16],
            b"SQLite format 3\0",
            "{name} is a plaintext SQLite file"
        );
        assert!(
            !bytes.windows(11).any(|w| w == b"interop-row"),
            "{name} contains its row bodies in the clear"
        );
    }
}

enum Keying {
    Passphrase,
    Raw,
}
