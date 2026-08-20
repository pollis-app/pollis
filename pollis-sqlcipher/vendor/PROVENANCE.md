# vendor/sqlite3.c

The SQLCipher amalgamation, **copied byte-for-byte** out of the crate this
workspace already builds everywhere else:

| | |
|---|---|
| Source | `libsqlite3-sys` 0.35.0, file `sqlcipher/sqlite3.c` |
| SQLCipher | 4.6.1 (community) |
| SQLite | 3.46.1 |
| SHA-256 | `f240498b2f20d6876905041417903b7fd837f964ad6c94c04727ef022f386151` |
| License | Public domain (SQLite) / BSD-3-Clause (SQLCipher, © Zetetic LLC) |

There is **no patch**. Everything that makes the Windows build different lives in
`../include/tomcrypt.h` and `../src/abi.rs`; the amalgamation is selected into
that provider purely by `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT` in `../build.rs`.
Reviewing this file therefore means checking one hash, not reading nine megabytes.

## Keeping it in step with `libsqlite3-sys`

`rusqlite` on Windows uses this library through `libsqlite3-sys`'s plain
`sqlcipher` (linked) feature, and takes its Rust declarations from that crate's
pregenerated `sqlcipher/bindgen_bundled_version.rs`. Those declarations were
generated against exactly this amalgamation, so the two must move together:

> **When bumping `rusqlite`/`libsqlite3-sys`, re-copy this file from the new
> crate's `sqlcipher/sqlite3.c` and update the hash above.**

Forgetting is not silent. `libsqlite3-sys` bakes `SQLITE_VERSION_NUMBER` into
its bindings and `rusqlite` checks it against `sqlite3_libversion_number()` on
first use, so a skewed copy fails loudly at runtime — the Windows CI job in
`.github/workflows/windows-link.yml` opens a database and would trip it.

To refresh:

```bash
cp ~/.cargo/registry/src/*/libsqlite3-sys-<ver>/sqlcipher/sqlite3.c \
   pollis-sqlcipher/vendor/sqlite3.c
sha256sum pollis-sqlcipher/vendor/sqlite3.c
```
