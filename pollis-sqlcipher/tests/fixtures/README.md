# Cross-provider SQLCipher fixtures

Two SQLCipher 4 databases that **a different implementation encrypted**, read
back by this crate's RustCrypto provider in
`a_database_another_implementation_encrypted_reads_back`.

| File | Keyed with | Exercises |
|---|---|---|
| `openssl-passphrase.db` | passphrase `pollis-interop-fixture-passphrase` | the full `kdf_iter`-round PBKDF2 that derives the page key |
| `openssl-rawkey.db` | raw 32-byte key `pollis-interop-fixture-key-32byt` | the shape `pollis-core` itself uses — raw page key, PBKDF2-derived HMAC subkey |

Each holds one table, `interop`, of 64 rows spanning six pages, so the reader
decrypts and MAC-checks pages past the salt-bearing first one.

## Why they are checked in rather than generated

Every other test in this crate writes with this provider and reads with this
provider, and that cannot tell correct SQLCipher from self-consistent nonsense:
a provider that derives the wrong key derives the *same* wrong key on both
sides, so the round trip succeeds and the file still is not SQLCipher 4. #1002
is the demonstration — a PBKDF2 mishandling `dkLen` ≠ `hLen` passed the entire
suite, `a_multi_page_database_round_trips` included.

A fixture only proves anything while the thing that wrote it is not the thing
reading it. These were written by `pollis-core`'s SQLCipher, which on Linux and
macOS is compiled against OpenSSL/CommonCrypto — so regenerating them from this
crate would quietly turn the interop test back into a round trip.

## Regenerating

Only if the contents or SQLCipher's parameters deliberately change, and **not on
Windows** (where `pollis-core` links this crate's provider, which is the one
thing the fixture must not come from):

```bash
cargo test -p pollis-core --lib --no-default-features \
    -- --ignored db::local::tests::regenerate_the_cross_provider_fixtures --nocapture
```

The generator lives in `pollis-core/src/db/local.rs`; its doc comment carries the
rest of the rationale. `.gitignore` needs its `!pollis-sqlcipher/tests/fixtures/*.db`
negation to keep these out of the blanket `*.db` rule.
