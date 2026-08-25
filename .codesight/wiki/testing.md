# Testing

Pollis has four tiers of automated tests:

1. **Unit tests** (in-crate `#[cfg(test)]` modules) — pure logic, in-memory rusqlite schemas, no I/O.
2. **Integration harness** (`src-tauri/tests/flows.rs`) — drives the real `pollis-core` commands end-to-end against a disposable test Turso database. Most of this document covers the harness.
3. **WebDriver E2E tests** (`e2e/*.js`) — drives the real shipped Tauri app (native WebKitGTK WebView, real Rust core) via `tauri-driver`. See [WebDriver E2E tests](#webdriver-e2e-tests-e2e) below.
4. **Playwright UI specs** (`e2e/*.spec.js`) — front-end interaction only, against the browser build with `VITE_PLAYWRIGHT=true` (Tauri IPC mocked in `frontend/src/__mocks__/`). No backend, seconds to run. See [Playwright UI specs](#playwright-ui-specs-e2especjs) below.
5. **Renderer unit tests** (`frontend/tests/*.test.ts`, `pnpm --filter frontend test`) — plain `node --test` over the renderer's pure functions, plus the source-scan guards. Milliseconds, no browser, no build step; Node's type stripping runs the TypeScript directly, so imports need the `.ts` extension.

### Source-scan guards, and when one is honest

Several of the renderer's tests read source files instead of driving the app.
That is the right shape when the thing being asserted is a rule **about the
code** rather than about what the code renders — where the symptom is invisible
to any behavioural test, or where the offending call sits somewhere no test can
reach:

| guard | the rule |
|---|---|
| `no-periodic-polling.test.ts` | no `setInterval` outside a reasoned allowlist (#874) |
| `query-store-boundary.test.ts` | no `queryFn` writes to a MobX store; the exports #929 deleted stay deleted |
| `message-window.test.ts` | no px load-more constant in `MessageList` (#934) |
| `ui-inventory.test.ts` | `.codesight/wiki/ui.md`'s inventory is regenerated, not hand-patched (#933) |
| `commands::r2::tests` (Rust) | the media-cache sweep never returns to window focus (#930) |
| `no-plaintext-temp-files.test.ts` | the renderer never writes a user's file to a temp dir; `writeFile` is only ever aimed at a save-dialog path; a queued attachment's bytes are a user file **or** staged, never a path this app invented (#1000) |
| `media_server::tests::serve_media_never_builds_a_response_body_itself` (Rust) | every response carrying decrypted media goes through the one builder that attaches `Cache-Control: no-store` (#1000) |

A guard is a poor substitute for a behavioural test and a good complement to
one. Each of the above is paired with something that exercises the real
behaviour — the rem threshold's arithmetic, the cap's actual eviction, the
inventory's round-trip, the staging registry's own lifecycle tests, a real HTTP
`GET` against a real spawned media server — so a guard passing on broken code is
not enough to make the suite green.

> The harness is built on top of Tauri's test machinery (`tauri::test::get_ipc_response` + `MockRuntime`). Tauri is the shipping shell, so the harness drives the real command logic through the same dispatch path the app uses, headlessly — `pollis-core` is the unit under test, reached through the `#[tauri::command]` shims exactly as at runtime.

## Integration harness — goals

The harness is the single source of truth for multi-client behavior that can't be unit-tested: invite acceptance, join-request approval, DM request flow, member removal, block enforcement, and full MLS encrypt/decrypt round-trips.

Design constraints:

- **Real command paths.** Tests go through `tauri::test::get_ipc_response` against a `MockRuntime` app, exactly as the React frontend does in production — no `_inner` shims, no mocked DB layer.
- **Per-client isolation.** Each `TestClient` owns its own `App<MockRuntime>`, its own `InMemoryKeystore`, and its own `AppState`. Two clients in one test cannot share keys or local state.
- **Shared remote.** All clients in a test round-trip through the same disposable Turso database, so read-after-write between clients exercises the same pipeline as two real users.
- **No timing, no websockets.** Tests drive the pipeline by explicit calls (`poll_mls_welcomes`, `process_pending_commits`) rather than waiting for LiveKit events.

## Running

```bash
cargo test --features test-harness --test flows
```

The `test-harness` feature:
- Exposes `Config::for_test()` (loads `.env.test`, stubs R2/LiveKit/Resend).
- Exposes `InMemoryKeystore` + the `test_harness` module.
- Pulls in `tauri/test` so `MockRuntime` is available.

It is **not** enabled in release builds.

### Running headless (no desktop environment)

```bash
cargo test -p pollis --no-default-features --features test-harness --test flows
```

The `pollis` crate's default features are the shipping desktop config —
`native-shell` (wry + webkit2gtk), `media` (LiveKit + libwebrtc + cpal/rodio),
and `os-keystore` (keyring/dbus). `--no-default-features` drops all three, so
the harness compiles and runs on `MockRuntime` on a machine with no display
server, no ALSA, and no dbus — a bare CI runner or headless box. The harness
never touches the shell or media surface, so coverage is identical; the
`[[bin]]` is `required-features = ["native-shell"]` and simply isn't built.
`pollis-core` has the matching `media`/`os-keystore` features (headless builds
use the file-backed keystore automatically — encrypted at rest since #882, never
plaintext).

**Keystore guard tests (#882).** Three of them, and they are guards, not
coverage:
- `keystore::file_backend::tests::guard_written_bytes_are_never_plaintext` — the
  bytes written to the store file must not contain the slot name or the value,
  and must not parse as the plaintext JSON store.
- `keystore::tests::guard_selected_backend_is_never_plaintext` /
  `guard_backend_choice_is_stable_within_a_process` — the runtime backend choice
  is one of two encrypting backends and is frozen per process.
- `pollis_tui::auth::tests::guard_cli_keeps_the_os_keychain_backend` — fails the
  TUI's own test run if `os-keystore` is dropped from its `pollis-core`
  dependency, which is exactly the #879 regression.

`pollis-core/tests/keystore_at_rest.rs` drives the shipped `OsKeystore` against a
real data directory and inspects the resulting file. It is its own test binary
because pointing the process at a directory is a whole-process act (see [the
data-dir section](#the-process-wide-data-dir--never-set_var)).

### What `mls-tests.yml` runs, in order

1. `cargo test -p pollis-schema` — the remote schema itself (#924). Seconds-cheap, no system deps, and it is the definition every other job stamps its databases from: it is the only automatic check that every migration file on disk is *listed*, that versions are unique and ascending, and that `single_db_scripts()` keeps main-then-log order (what a single-DB deploy depends on).
2. `cargo test -p pollis-api` — the client/server wire contract (#875, #922), whose `compile_fail` doctests are the guarantee that a mistyped request **or response** body cannot compile.
3. `cargo test -p pollis-delivery` — DS serializer + route coverage.
4. `cargo test -p pollis-core` — MLS unit tests, and the local-database at-rest tests (#992): `the_local_database_file_is_encrypted_at_rest` writes a message through `LocalDb::open_for_user` and then reads the file's raw bytes back, and `the_crypto_provider_is_the_one_this_platform_documents` (#998) asks `PRAGMA cipher_provider` whether the build got the provider `docs/security-whitepaper.md` §7.0 documents for this platform.
5. `cargo test -p pollis-sqlcipher` — the SQLCipher crypto provider the **Windows** build links (#992): known-answer vectors (RFC 6070, RFC 4231/2202, NIST SP 800-38A) plus a real encrypted database driven straight through the vendored amalgamation. It only ships on Windows, but its C is compiled on every target precisely so this runs here — a provider mistake fails in seconds on Linux rather than 25 minutes later in `windows-link.yml`.

   **What a round trip cannot check (#1002).** Writing with this provider and reading it back does not distinguish correct SQLCipher from self-consistent nonsense: a provider that derives the wrong key derives the *same* wrong key on both sides, so the data comes back and the file still is not SQLCipher 4. Three separately-built broken providers — a counter PRNG, a PRNG filling half the buffer it reports, and a PBKDF2 mishandling `dkLen` ≠ `hLen` — passed all 14 tests this crate had, `a_multi_page_database_round_trips` included. Two things close that, and both are worth preserving if you touch `src/abi.rs`:

   - **KDF vectors on both sides of `hLen`.** Every original PBKDF2 vector derived exactly one hash block (`dkLen == hLen`), so `T_2` was never computed and the truncation path never taken. `pbkdf2_derives_lengths_that_are_not_one_hash_block` covers SHA-1 at 16 and 25 bytes and SHA-512 at 32 and 100. `dkLen < hLen` is not academic: SQLCipher derives the HMAC subkey as 32 bytes out of SHA-512 on **every** connection, raw-keyed ones included.
   - **Cross-provider fixtures.** `tests/fixtures/*.db` are SQLCipher databases written by `pollis-core`'s OpenSSL/CommonCrypto-backed SQLCipher and read back here by the RustCrypto provider. This is the only check in the crate whose writer and reader are different implementations. Regenerate only deliberately, and never on Windows — see `tests/fixtures/README.md`, since a fixture this crate generated would silently be a round trip again.

   PRNG assertions get the same treatment: `fortuna_writes_every_byte_it_reports` pre-fills a sentinel and proves every reported byte was written (a partial fill leaves a non-zero prefix that carries the older, weaker assertions), and `fortuna_does_not_merely_count` scores each byte *position* across 64 reads, because a counter varies only in its low-order bytes.
6. `cargo test -p pollis-tui` — see below.
   - `cd mobile && pnpm test` — Node's own runner over the pure modules in the Expo app (`mobile/tests/`). Mobile is a standalone project, not a workspace member, so it is its own command; it runs in CI inside `mobile-core-check.yml`'s cheap `expo-doctor` job rather than behind the 3-ABI `android-build`. Today it covers `applyReactionToggle`, the optimistic reaction reducer — whose whole job is to predict what `useConversationReactions` will report, so the cases that matter are the ones where the two could disagree (pill ordering, a message dropping out of the map, `count` tracking `user_ids`, an already-true toggle).
7. `cargo test --features test-harness --test flows` — the integration harness.
8. `cargo clippy --workspace --all-targets -- -D warnings` — the Rust lint gate (#912). Runs last so the test signal lands first, and inside this job because the toolchain, apt deps and cargo cache are already there. `rust-toolchain.toml` lists `components = ["clippy"]`, so `cargo clippy` works on a dev box with no extra step; the job pins `dtolnay/rust-toolchain@1.96.0` (the action exports `RUSTUP_TOOLCHAIN` and would otherwise override the pin) so a new stable's new lints cannot turn an untouched PR red.

Every step after the first carries `if: ${{ !cancelled() }}`, so one failure still lets the rest report.

The workflow has a second heavy job, `macos-at-rest` (#998), gated on the same
`changes` decision and reported by the same `gate`. It runs
`cargo test -p pollis-core --lib --no-default-features -- db::local::tests::` on
`macos-latest` and greps the log for the four load-bearing test names, exactly
as `windows-link.yml` does. It exists because until #998 **no macOS runner in
this repo executed a single test** — the only macOS jobs were
`mobile-core-check`'s iOS cross-compile `cargo check` and the tag-triggered
release builds (desktop, CLI, verifier), none of which runs a test — while macOS
is the one desktop platform whose SQLCipher crypto
provider is CommonCrypto, chosen by `libsqlite3-sys`'s build script and
flippable to OpenSSL by an `OPENSSL_DIR` on the runner. `--no-default-features`
drops `media` and `os-keystore`, neither of which the at-rest path touches;
`cargo tree -i libsqlite3-sys` is identical with and without `media`, so the
"which sqlite3 won the symbols" question is unchanged, and the job stays minutes
long on a runner billed at 10x.

### What `windows-link.yml` runs (#991, #992)

Everything above is Linux. `.github/workflows/windows-link.yml` is the only gate
that meets the **MSVC linker** before a release does: it builds `pollis-core`
for `x86_64-pc-windows-msvc` and links the `cdylib`, then runs — natively on the
target — `cargo test -p pollis-sqlcipher` and the whole `db::local::tests::`
module. That second half is #992's evidence and is a *runtime* question no link
can answer: `sqlcipher_is_the_sqlite_we_actually_linked` (SQLCipher is present),
`the_local_database_file_is_encrypted_at_rest` (a message written through
`LocalDb::open_for_user` does not appear in the file's bytes, the file does not
begin with the plaintext SQLite header, and neither an unkeyed nor a
wrongly-keyed connection can read it), and
`a_plaintext_database_is_destroyed_not_opened`, plus
`the_crypto_provider_is_the_one_this_platform_documents` (#998). The step greps
for each of those four names, because libtest exits 0 when a filter matches
nothing and a rename would otherwise turn the whole check into a silent no-op.

It exists because `build-windows` used to live only in the tag-triggered
`desktop-release.yml`. v1.10.0 was cut from a fully-green PR whose Windows link
could not resolve at all — 1,253 duplicate symbols between SQLCipher's OpenSSL
and libwebrtc's BoringSSL, plus two unresolved C-runtime imports — and no PR
check could have seen it. A `cargo check` would not have caught it either:
**this entire failure class only exists at link time.** Same three-job shape as
`mls-tests.yml`; `windows-gate` is the always-reporting check, `link` is the
heavy job. It also runs on pushes to `main`, which is the only trigger allowed to
write the Windows rust-cache.

### What the flows harness's local database is (#992)

The flows binary runs the Delivery Service **in-process**, so it links `libsql`
— and therefore a second sqlite3 amalgamation — beside `pollis-core`'s
SQLCipher. The linker gives `libsql`'s the `sqlite3_*` symbols, SQLCipher's
object is never pulled, and `PRAGMA key` is an unknown pragma there, which
SQLite ignores silently. **Every client in the flows suite runs on an
unencrypted local database.**

That is a property of the harness, not of the product: `pollis-core`,
`pollis-tui` and the desktop binary contain exactly one sqlite3 (#987/#988 took
`libsql` out of the client for this among other reasons), and
`db::local::tests::the_local_database_file_is_encrypted_at_rest` is what proves
the shipped file is ciphertext. A green flows run is not evidence either way,
which is why `tests/flows/linked_sqlite.rs` asserts the harness's state in both
directions rather than leaving it to be rediscovered — if `libsql` ever leaves
this binary those two tests fail, and the right response is to delete them.

It also matters to `db::local::destroy_plaintext_database`, which disposes of
legacy plaintext databases (#992). That step is gated on
`sqlcipher_is_linked()`: destroying a plaintext file is an upgrade when the
replacement will be encrypted and pure data loss when it will not, and in this
binary it would loop forever — zeroing, on every open, the one
`pollis_{user_id}.db` that two simulated devices of the same user share, out
from under whichever of them still has it open.

### pollis-tui in CI (#487)

`.github/workflows/mls-tests.yml` also runs `cargo test -p pollis-tui` — the
terminal client's unit tests plus its headless in-process-DS **smoke rig**. The
TUI drives `pollis-core`'s sync path directly (no Tauri/IPC), so a change to that
path could regress it unnoticed; gating it in CI catches that. The smoke rig
applies the same `POST_BASELINE_LOG_MIGRATIONS` (the post-baseline commit-log-DB
migrations) as the app, so it exercises the current log-DB schema rather than a
stale baseline. See [pollis-tui.md](./pollis-tui.md).

## No `.env.test`, and no database to point it at (#987)

The suite needs no configuration. `Config::for_test` reads nothing from the
environment and the harness backs "remote Turso" with two process-local libsql
files it creates in a tempdir — which it has done since #420; what #987 removed
is the last reason a *credential* had to be present at all. `TURSO_URL` /
`TURSO_TOKEN` used to be required fields of `Config`, so CI provisioned an
`.env.test` containing `libsql://placeholder.invalid` and the literal string
`placeholder` to satisfy them. Both fields are gone, so both the file and the CI
step that wrote it are gone with them.

`cargo test --features test-harness --test flows` therefore runs green with every
`TURSO_*` and `LOG_DB_*` variable unset — which is the property #987 claims, and
now a structural one rather than a convention.

The harness wipes all tables at the start of each test (see `wipe_remote` in
`src-tauri/src/test_harness.rs`), and tests serialize on a process-wide mutex
(`serial_test`) because that wipe would race otherwise.

## Architecture

### `TestWorld`

Lazy, process-wide singleton (`tokio::sync::OnceCell`) that owns:

- Two `Arc<pollis_delivery::db::Db>` handles — the main DB and the commit-log DB,
  two genuinely separate local libsql files. They belong to the in-process DS;
  since #987 a client has none of its own, so tests that must construct
  server-side state reach them through `harness::writable_remote()` /
  `writable_log()`.
- `Config` — built by `Config::for_test`, which reads NOTHING from the
  environment since #987 (it used to require `TURSO_URL`/`TURSO_TOKEN` out of a
  CI-provisioned `.env.test`; there is no such field to fill any more).
- A tempdir used for per-user SQLCipher files, installed with `db::local::set_data_dir`.

### `TestClient`

One simulated device for one user. Owns:

- `App<MockRuntime>` + `WebviewWindow<MockRuntime>` — the headless Tauri runtime.
- `Arc<AppState>` — wired with an `InMemoryKeystore`. No database handle: `pollis-core` does not link `libsql` (#987), so every read this client performs is a signed POST to the in-process DS.
- `profile: Option<UserProfile>` — populated after `sign_up`.

**Two devices of the same user SHARE local state.** `LocalDb::open_for_user` derives
the path as `pollis_{user_id}.db`, keyed on user_id only, so a `TestClient` built by
`enroll_second_device` opens the *same* local SQLite file — and therefore the same MLS
state — as the primary. It never gets its own leaf in the tree, and `contents()` on it
just reads plaintext the primary already decrypted. Any assertion of the form "this
user's *other* device cannot decrypt X" is therefore **vacuous** here and will pass or
fail for reasons unrelated to MLS. Use a second *user* when you need genuinely separate
local state and a real second leaf. (Found while writing the #679 regression test.)

### `invoke<T>`

Wraps `tauri::test::get_ipc_response` in `spawn_blocking` (it uses `std::sync::mpsc` internally so it cannot run on the async runtime directly). Takes a webview, command name, and JSON args; returns the command's `Result<T, String>`.

### Schema bootstrap

The harness embeds `remote_schema.sql` + every numbered migration via `include_str!` and applies them idempotently at first use (tracked via the `schema_migrations` table). There is no dependency on `pnpm db:push` — a fresh Turso database works out of the box.

Known prod schema drift is applied in `apply_drift_fixups` (currently: `group_invite.status` column). Keep this list short; drift should be fixed in a real migration, not codified here.

## DEV_OTP

`DEV_OTP` is the fixed code `000000`: `verify_otp` short-circuits the email send and accepts it, so no real emails are sent and no OTP storage round-trips through Resend.

It is passed **explicitly**, never through the environment (#923): the in-process DS is constructed with `OtpConfig { dev_otp: Some(DEV_OTP), .. }` and every client posts the constant to verify-otp. The harnesses used to also `set_var("DEV_OTP", ..)`, which changed no behaviour (the client-side default is the same code) and only added a `setenv` race.

## The process-wide data dir — never `set_var`

Everything a device keeps on disk (the local SQLCipher DB, `accounts.json`, the file keystore, the overlay guard book) hangs off one directory, resolved by `pollis_core::db::local::dirs_path()`.

Test rigs need to point that somewhere disposable, and a multi-device rig needs to *swap* it between simulated devices. Do it with **`pollis_core::db::local::set_data_dir(dir)`**, never `std::env::set_var("POLLIS_DATA_DIR", ..)`:

- `setenv` mutates a process-global array that any other thread's `getenv` may be reading at that instant. That is a data race — the reason Rust 2024 made `set_var` `unsafe` — and these rigs always have other threads live (the in-process DS, background sync loops, `spawn_blocking` keystore work). It fails rarely, under load, with nothing broken (#923).
- `set_data_dir` is a locked write behind an `RwLock`, so a reader always sees one whole path.

`POLLIS_DATA_DIR` still works and is still what a second dev instance and the mobile bridge use; the override just takes precedence. Production never sets it.

## Writing a scenario

```rust
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn my_scenario() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Drive commands via helpers on TestClient, or use invoke_json directly
    // for commands without helpers.
    let group_id = alice.create_group("My Group").await;
    alice.invite(&group_id, &bob_profile.username).await;

    // Accept, poll MLS welcomes, then assert.
    let invite_id = bob.first_pending_invite().await.unwrap()["id"]
        .as_str().unwrap().to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    assert!(alice.group_member_ids(&group_id).await.contains(&bob_profile.id));

    drop(alice);
    drop(bob);
}
```

Rules of thumb:

- Every scenario starts with `wipe().await` — tests assume a clean remote.
- Always `#[serial]` — the wipe races otherwise.
- Always `flavor = "multi_thread"` — single-threaded tokio deadlocks because `spawn_blocking` needs worker threads.
- `drop(client)` at the end forces the borrow checker to keep the client alive through the final assertions; without it, a client may be dropped early in some builds, closing its local DB mid-assertion.

## libsql local DBs cannot go in pollis-core's `--lib` tests

libsql's local backend calls `sqlite3_config` on first use, which returns
`SQLITE_MISUSE` (21) once rusqlite/SQLCipher — what `LocalDb` uses — has already run
`sqlite3_initialize` in the same process. A lib test that opens a local libsql DB
therefore **passes under a filter and panics in the full suite**, depending only on
whether it shares its binary with a test that touched the local DB.

This shaped where `pollis-core`'s remote-query tests lived: in `tests/`, since an
integration binary gets its own process and never loads rusqlite.

**Since #987 the question does not arise for `pollis-core` at all** — it does not
depend on `libsql`, so no test of its can open a local libsql DB by accident, and
`tests/no_client_side_remote_reads.rs` fails the build if the dependency ever
returns. The remote-query tests moved to `pollis-delivery` with the queries
themselves (`tests/registered_devices.rs` is the #679 suite), and the constraint
still applies THERE — but harmlessly, because that crate never links rusqlite.

## `pollis-delivery` DB fixtures — use `common::TempDb` (#942)

Every DS integration test that needs a real database gets it from
`pollis-delivery/tests/common/mod.rs`:

```rust
mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}
```

`TempDb` owns the database **and** the temp directory, and derefs to `Arc<Db>`, so
`db.conn()`, `Arc::clone(&db)` and `&db` where a `&Db` is wanted all work
unchanged. The one rule is to **hold it for the whole test** rather than let it
drop early — which binding it to a `let` already does. Where a fixture hands back
something that borrows the DB (a `Router`, a `Connection`), return the guard
alongside it so the caller keeps it alive.

Do **not** reach for `std::mem::forget(dir)`. Twenty-one fixtures used to, because
a `TempDir` deletes its directory on drop and dropping it at the end of the
builder pulled the ground out from under a database the test had not started using
yet. Leaking was the shortest fix and a permanent one: every test binary left a
directory in `/tmp` on every run, and nothing ever removed them. Ownership solves
the same problem without the leak. `pollis-delivery/tests/fixtures.rs` asserts the
directory really is removed on drop, so a regression to leaking fails a test rather
than quietly filling the disk.

## Contract tests that fail at COMPILE time

Three of this repo's invariants are checked by code that must *fail to build*
rather than by an assertion, which makes them cheap and impossible to skip.

- **`pollis-api` `compile_fail` doctests (#875, #922).** A request body missing a
  field is `E0063`; a misspelled one is `E0560`; a response field the server does
  not send is `E0609`; answering an endpoint with another endpoint's body is
  `E0308`; posting an operator-only endpoint from `pollis-core` is `E0277`.
  Each has a matching *positive* doctest next to it — a `compile_fail` test whose
  code could never compile for an unrelated reason proves nothing.
- **`pollis-delivery`'s `ok_response` doctest (#922).** The same `E0308`, on the
  real server-side helper rather than a stand-in.
- **`pollis-core/tests/no_client_side_remote_writes.rs` (#910).** Not a compile
  failure, but the same spirit: it parses the remote table list out of
  `pollis-schema` and fails if any non-test code in `pollis-core` writes one.
  `CLAUDE.md`'s "never add a client-side remote INSERT/UPDATE/DELETE" had one
  live counter-example until #910; this is what keeps the count at zero.

## Measuring memory, not guessing (#915)

`pollis-core/tests/attachment_memory.rs` installs a counting `#[global_allocator]`
and asserts how much `cache_encrypt` allocates for a 16 MiB payload. It needs its
own process — which an integration test binary already is — so the allocator does
not perturb anything else.

Assertions are in multiples of the payload, never absolute bytes, so they keep
meaning something as constants change. The pre-#915 implementation allocated two
full payloads on the tight-buffer path and three on the real one; both are pinned.

## Performance guards (#875)

Four tests pin the SHAPE of hot paths rather than their wall clock, because a
statement count is identical on every machine and a timing is not. All four are
**guards, not proofs**: they cannot tell a fast path from a slow one, only that
the structure that made it slow has not come back.

| Guard | Where | What it forbids |
|---|---|---|
| `a_run_of_envelopes_loads_the_group_once` | `mls/tests.rs` | Reloading the whole MLS group per decrypted envelope (was 12 `mls_kv` SELECTs + 1 UPDATE per message; now 12 for the run). Also asserts the per-message secret-tree UPDATE is still there — that write IS forward secrecy. |
| `no_crate_builds_its_own_reqwest_client_outside_the_seam` | `pollis-relay/src/http.rs` | Any crate calling `reqwest::Client::new()`/`::builder()` outside the three sanctioned factories. A fresh client has an empty connection pool, so each call re-handshakes. |
| `no_lock_guard_in_a_scrutinee_spans_an_await` | `pollis-core/src/state.rs` | The edition-2021 footgun where a guard taken in an `if let`/`match` scrutinee stays alive for the whole body, so `.clone()` only *looks* like it released the lock. |
| `envelope_fetch_sql_tests` | `messages/ingest.rs` | The batched envelope fetch degenerating back to one query per conversation, and the per-conversation watermark correlation being lost. |

`MlsStore` counts its own statements under `test-harness`
(`signal::mls_storage::counters`). The counters are **thread-local**, not
process-global: `cargo test` runs tests concurrently in one process, so a global
counter would have every test's storage traffic bleeding into every other test's
measurement. That means a measured section must not contain an `.await` that can
hop threads — every MLS storage call site is a synchronous block, which is what
makes the numbers reproducible.

`measure_decrypt_statement_count` prints (rather than asserts) the before/after
figures behind the wiki's MLS numbers:

```bash
cargo test -p pollis-core --no-default-features --features test-harness \
  --lib measure_decrypt -- --nocapture
```

## Extending the harness

- **Add a helper to `TestClient`** when a command is used from more than one scenario, so assertions stay readable.
- **Register new commands in `build_client_app`.** The `tauri::generate_handler![...]` macro call in `src-tauri/src/test_harness.rs` must include every command invoked by a test.
- **Add FK-safe wipes.** If you introduce a new table referenced by tests, add it to `wipe_remote` in the correct order (child tables before parent).

## The in-process Delivery Service — the REAL router (#918)

`spawn_in_process_delivery` (in `harness.rs`) boots **`pollis_delivery::
build_router_with_state`** — the production router, verbatim — on a loopback port,
against the harness's own libsql handles. Every DS call a flows test makes lands
on the shipped handler.

It did not always. The harness used to re-declare the route table and wrap all ~60
handlers itself, because its DB handle was a `pollis-core` `RemoteDb` and the DS
wanted a `pollis_delivery::db::Db`. #925 bridged the two with
`RemoteDb::shared_database()` + `Db::from_shared()` so both wrapped the SAME
libsql `Database`; #987 removed the bridge along with `RemoteDb` itself, and the
world now simply owns `Db` handles and hands them to the DS. Opening a second
`Builder::new_local` on one file is still NOT equivalent: two independent handles
do not share WAL writes promptly, and a test reading rows the DS "already wrote"
is exactly the ghost failure this suite exists to rule out.

`pollis-tui/tests/common` made the same move in #987, for a sharper reason: with
every client READ now a DS endpoint, a hand-rolled double would have had to grow
twenty more handlers whose only job was to agree with the originals. It mounts
`build_router_with_state` too, and ~900 lines of second implementation went.

The copy was not free while it lasted. It silently omitted ten routes:
`/v1/invite-links/redeem` 404'd under test while working in production, and
custom emoji (#848) had no integration coverage at all. Two tests keep the mirror
from coming back — `flows/ds_surface.rs::every_declared_endpoint_is_reachable`
and `pollis-delivery/tests/endpoint_coverage.rs::every_declared_endpoint_is_routed`
— both walking `pollis_api::ENDPOINTS`, the single table the client and both
routers are built from.

**Auth is ENFORCED** in the harness DS (`require_auth = true`), so the suite drives
the signed write path end to end. Per-IP rate limits are raised (every request in
the run comes from 127.0.0.1, which the production limits read as one abusive
client); `ratelimit.rs`'s own unit tests pin the real numbers.

## The `DsFault` seam — injecting DS-side faults

`/v1/commits` is the ONE path the harness serves itself, mounted on an outer
router whose `fallback_service` is the real one — so the override is an
*addition*, not a re-declaration, and every other endpoint (today's and
tomorrow's) reaches production code with no edit to the harness. That seam is the
only place a fault can be injected *without* touching production code:
the client's `SubmitResult` is lossy (it discards the DS's `Rejected` detail) and
`http_submit` is hardwired, so there is no client-side network seam to mock. All
faults therefore live **DS/harness-side**, and the real client code path runs
untouched against a DS that behaves exactly like prod plus one perturbation.

`harness::DsFault` is a **one-shot, atomically-consumed** fault menu (a
`Mutex<Option<DsFault>>`, `NEXT_DS_FAULT`). Arm it with `arm_ds_fault(fault)`
immediately before the operation whose commit submit you want to perturb; it
fires exactly once and disarms, so it is safe even though every `#[serial]` test
shares the single in-process DS. Assert it fired with `!ds_fault_armed()`.

| Fault | What the DS does | Client must |
|---|---|---|
| `DropResponse` | commit + GroupInfo + Welcomes **land**, then the success response is turned into a 500 (the #411 shape) | adopt its own canonical commit (`our_commit_is_canonical`), not wedge |
| `Fail500PostWrite` | same as above — persists, then 500s | adopt (generalized #411, e.g. on a *removal* commit) |
| `Fail500PreWrite` | returns 500 **without persisting** — the clean, retryable failure | roll the staged commit back cleanly; never adopt a phantom epoch |
| `DropWelcome` | commit + GroupInfo land, but the add's Welcomes are **never persisted** (cleared off the body before `submit_commit`) | recover the new member via external-join |

The gap-creating fault is **not** a `DsFault` variant. A live submit-path deletion
of the *head* commit row only lowers the head (the next committer refills it), so
it never leaves a durable interior gap. Instead, `harness::drop_commit_row(conv,
epoch)` deletes one interior row **post-hoc** on the LOG DB (the handle the DS
itself writes through) after higher epochs have already been appended — the
`… N-1, [gap], N+1 …` shape that trips `process_pending_commits`'s append-only-log
gap detector. It asserts loudly that exactly one row was removed. `conv` is the
**MLS group id** — for a group channel that is the `group_id`, not the channel id
(`process_pending_commits` resolves channel→group). `harness::ds_head_epoch(conv)`
reads `MAX(epoch)+1` from the log via `pollis_delivery::commit::head_epoch` for
convergence assertions.

**Production `pollis-delivery` semantics are never modified** by any of this — the
faults are pure harness-layer perturbations (`delivery_submit` consumes them; the
gap helper writes directly to the shared LOG handle).

## The adversarial recovery suite (`flows/adversarial.rs`)

These scenarios follow the backend-core doctrine: each tries to *create* an
invalid / lossy state and proves the group either refuses it or converges out of
it (never happy-path replay). Convergence is asserted through the real command
pipeline — a fork, wedge, or squatted duplicate leaf fails the decrypt checks.

- **`fail500_post_write_commit_is_adopted_not_wedged`** — generalized #411 on a
  **removal** commit. `Fail500PostWrite` makes the DS persist alice's carol-removal
  + GroupInfo then 500; alice must observe her commit is canonical and adopt it.
  Proves: fault fires once; roster converged (carol gone, alice+bob remain);
  alice not wedged (bob decrypts her post-adopt message); evicted carol can't read it.
- **`fail500_pre_write_persists_nothing_and_does_not_wedge`** — the contrast case.
  A pre-write 500 must persist nothing, so the commit-log head is **unchanged**
  and alice rolls back cleanly (no phantom epoch): she still round-trips with bob
  at her real epoch. This is the "pre-write ≠ lost-response" distinction.
- **`epoch_gap_recovers_via_external_join`** (#430-P2 / F1) — bob is offline while
  the group churns through several epochs, then one interior commit row is dropped
  (`drop_commit_row`). On return bob retains the message at his join epoch
  (decrypted by the interleave hook *before* the gap), trips the gap detector,
  forgets his stale group, and external-joins onto the head; he then decrypts
  post-recovery traffic and the group agrees on the head epoch. **Accepted loss
  (documented, not fought):** messages sealed at the epochs the gap forces bob to
  jump over are unrecoverable for bob — that is the injected F1 gap's direct
  consequence, and exactly what the I1 DB triggers exist to prevent upstream. The
  test proves the client *recovers*, not that the gap is lossless.
- **`dropped_welcome_recovers_via_external_join`** (F5) — bob's add commit +
  GroupInfo land but `DropWelcome` strips his Welcome. With no Welcome to drain,
  bob's catch-up finds no local group and external-joins from GroupInfo, creating a
  *second* leaf; the staying member must prune the duplicate. Proves both-direction
  decrypt (a fork would break one) and a roster listing bob exactly once. The
  GroupInfo-**and**-Welcome-both-dropped case is accepted as unrecoverable and is
  not attempted here.
- **`eviction_then_readd_has_provable_blackout`** — bob reads a pre-removal
  message (cached locally), is removed while offline, and two messages are sent
  while he is out; he is then re-added (`apply_welcome` deletes his stale group and
  rejoins him at the new epoch). He decrypts the cached pre-removal message and
  post-re-add traffic but **provably cannot** decrypt the two evicted-window
  messages (sealed at epochs he was not a member of). Note bob stays passive during
  the blackout — a merely-*removed* (non-revoked) device that processed its removal
  would external-join back in; keeping bob offline is what makes the blackout real.
- **`revoked_device_locked_out_of_every_recovery_path`** — bob's `user_device` row
  is tombstoned (`revoked_at`) and he is removed, then he drives **every** client
  recovery entry point (`process_pending_commits`, `get_channel_messages`). The
  `local_device_registered` gate keeps him out of external-join; the load-bearing
  checks are the *observable* lockout (he can't decrypt any post-removal message,
  and is absent from the roster) plus the group staying live for carol — a silent
  gate no-op is never accepted as a pass, and a wedge would fail carol's check.
  **Scope note:** it revokes *and* removes, so the leaf is pruned by the explicit
  `remove_member` — it proves recovery paths stay shut, not that revocation itself
  prunes anything. That is its sibling below.
- **`revoking_one_device_prunes_its_leaf_with_no_roster_change`** (#679) — the
  no-roster-change half. Bob is asserted to be a genuinely decrypting member first,
  then his `user_device` row is tombstoned with **no `remove_member`** — that single
  omission is the whole test, since any roster change prunes the leaf by removal and
  never exercises revocation. Alice sweeps (the reconcile backstop, whose
  `local_tree_has_stale_leaf` pre-check and `desired` set both read
  `registered_devices`), and bob must stop decrypting while carol, untouched, still
  can. A final check asserts bob is **still a roster member**, so the eviction was
  device-scoped rather than a removal in disguise. Uses three distinct users on
  purpose — see the shared-local-state warning under `TestClient`.
- **`cross_channel_sibling_message_is_not_stranded`** — regression for the
  cross-channel epoch strand. Carol is a continuous member of a group with two text
  channels A and B. Alice sends `mB0` on B (carol hasn't fetched B), then adds bob
  (a commit advancing the *shared* MLS group). Carol opens A first — a per-channel
  catch-up would advance the shared group past `mB0`'s epoch and drop it
  (`max_past_epochs = 0`). The group-level `catch_up_mls_group_interleaved` instead
  catches up **every** sibling channel when A is opened, so `mB0` is decrypted at its
  epoch and visible when carol later opens B. Proves the group-level catch-up closes
  the strand that a bare per-channel/commit-only replay leaves open.
- **`committer_does_not_strand_inbound_message`** — regression for the *committer
  strand* (#440), the commit-INITIATION variant. Carol sends `m0` while alice (a
  member) hasn't fetched it, then alice adds bob. Advancing her own epoch by merging
  the add commit would discard `m0`'s keys (`max_past_epochs = 0`) before she ingests
  it. The fix makes the pre-op paths (send / edit / invite / remove) run the
  interleaved ingesting catch-up **before** advancing their own epoch, so alice
  decrypts `m0` before the add. Proves the group-level catch-up (fetch/sweep/realtime)
  and the pre-op ingest (commit-initiation) together leave no path that strands a
  current member's in-window message.

## The model-based proptest fuzz layer (`flows/model.rs`)

Where `adversarial.rs` proves *hand-picked* recovery orderings, `model.rs` is the
"beyond reasonable doubt" complement: it **generates random op/fault/offline
sequences**, forces the group to converge, and asserts the bulletproof-membership
invariant for *every* generated sequence. It is model-based (not blind fuzzing)
because it maintains a plain-Rust **shadow oracle** alongside execution and checks
reality against it.

### The shadow oracle

Over a fixed pool of 4 pre-signed-up clients (`alice` = the owner/committer, plus
`bob`/`carol`/`dave`) and one group channel, it tracks:

- the **current membership set**, and each current member's **continuous-stint
  join clock** (`joined_at`), and
- for every Send, `(body, membership_snapshot, sent_at_clock)`.

After convergence it asserts, for each actor `X` and message `M(snapshot S,
clock t_M)`:

1. **Positive delivery** — if `X` is a current member AND has been a member
   *continuously* since `M` (its stint's join clock ≤ `t_M`), `X` MUST decrypt `M`.
2. **Negative / forward secrecy** — `X ∉ S` ⟹ `X` must NOT decrypt `M`. This
   encodes *exactly* the two accepted losses (before-join; sent-while-removed) and
   nothing weaker.
3. **No wedge** — every current member decrypts a final alice-authored probe
   (per-client proof they all reached the head; stronger than reading the
   server-side `ds_head_epoch` integer, which is also sanity-checked).
4. **Roster consistency** — every current member's `group_member_ids` equals the
   shadow model's current set.

Deliberately **not** asserted (to be neither too strong nor too weak): a removed
member's *cached* history (`X ∈ S` but not current), and messages sent during a
stint `X` was later removed-and-re-added from (`X ∈ S` but current join clock >
`t_M`) — removal forgets MLS state and re-add gives a fresh leaf, so an unfetched
pre-removal message is cryptographically gone and there is no key backup
(Megolm-style backup is forbidden). The *cached* flip side is covered
deterministically by `eviction_then_readd_has_provable_blackout`.

### Ops → real commands (no invented seams)

Each op maps to a method already used by the green suite; there is no
rotate/self-update op because `self_update` does not exist in this repo. Ops that
are ill-formed against the shadow model (Send from a non-member, Remove of an
absent actor, Add of a present one) are **skipped in execution and not recorded**.

| Op | Backing command(s) |
|---|---|
| `Add(t)` | `join_member`: `send_group_invite` → `accept_group_invite` → `poll_mls_welcomes` → `process_pending_commits` |
| `Remove(t)` | `remove_member_from_group` + committer `process_pending_commits` |
| `Send(a)` | sender syncs (`poll_mls_welcomes` + `process_pending_commits`), then `send_message` |
| `Sync(a)` | `poll_mls_welcomes` + `process_pending_commits` + `get_channel_messages` (models "come back online") |
| `Fault(v)` | `arm_ds_fault` before the next commit-producing op |

`process_pending_commits` and `get_channel_messages` both route through the
group-level `catch_up_mls_group_interleaved`, so a returning member decrypts every
message sealed at an epoch it advances past — not a bare commit-only replay. This
is what makes the "member continuous since `M` was sent must decrypt `M`" assertion
pass under offline-churn: without the interleave, `process_pending_commits` would
jump the shared group to head and strand `M` (`max_past_epochs = 0`).

The alice-committer ops (`Add`/`Remove`) also strand `M` on the **commit-initiation**
side if alice commits while holding an un-ingested inbound `M` at the current epoch
(the committer strand, #440): `send_group_invite` / `remove_member_from_group` run
the interleaved catch-up **before** entering `reconcile_group_mls_impl` (which holds
the `mls_group_lock` for its whole body — so the catch-up is hoisted above the lock
to avoid a re-entrant deadlock), and `send_message` swaps its commit-only catch-up
for the interleaved one. This is why the fuzzer, which previously surfaced this class
and was `#[ignore]`d, is now re-enabled and green.

The fuzzer's fault set is the three **landing** faults — `Fail500PostWrite`,
`DropResponse`, `DropWelcome` — all of which leave the commit durable and drive the
client through a recovery path (adopt-own-canonical / external-join / duplicate-leaf
prune) while the command still returns `Ok`, so the shadow model applies the
membership change normally. `Fail500PreWrite` (clean no-op rollback, surfaces a
client error) and the `drop_commit_row` interior gap are left to their deterministic
scenarios (`fail500_pre_write_persists_nothing_and_does_not_wedge`,
`epoch_gap_recovers_via_external_join`).

### The hard parts (how they're handled)

- **Async/proptest bridge** — proptest closures are sync; the harness is async.
  One process-wide multi-thread `tokio::runtime::Runtime` `block_on`s the case body
  inside the closure. The whole test is `#[serial]` (shared `WORLD` / in-process DS
  / `NEXT_DS_FAULT`); every case starts with `wipe()` + `clear_ds_fault()` so cases
  can't bleed. `clear_ds_fault()` is a harness helper added for exactly this.
- **Determinism / shrinking** — MLS key generation uses the OS RNG and is **not
  seedable** from the harness, so replays aren't bitwise-identical and shrinking is
  best-effort (we do **not** claim deterministic shrinking). Failure persistence is
  disabled (`failure_persistence = None`) so no misleading "regression" seed is
  written; every failure message embeds the full **op sequence**, which is the repro
  of record.
- **CI time budget** — each case spins real MLS crypto + a real DS, so CI runs a
  **modest** count (`DEFAULT_CASES = 32`, sequences of 4–12 ops, 4 actors) — a couple
  of minutes. This is documented, not silent under-coverage. Deep fuzzing is a local
  soak: `PROPTEST_CASES=2000 cargo test --features test-harness --test flows model`.
- **The oracle must read like production** — the delivery assertions fetch each
  member's view through the same paginated `get_channel_messages` the app uses,
  and the harness pages through the **full** history (`fetch_channel_messages`
  follows `next_cursor` to the end). A single-page read looks correct until a
  run sends more than one page (50) of messages, then falsely reports the
  *oldest* messages as lost for a member who has been present since the start —
  they're simply below the first page. That artifact burned real time as a
  suspected production loss bug (#442) before being traced to the oracle.

### The marathon soak (`model_marathon_convergence`)

One long randomized sequence instead of many short ones — the shape that
surfaces slow-burn divergence (deep epoch churn, long offline stints, fault
pile-ups) that 4–12-op cases can't reach. It is `#[ignore]`d (multi-minute);
run it explicitly:

```bash
MARATHON_OPS=500 MARATHON_ACTORS=8 cargo test --features test-harness \
  --test flows -- --ignored --nocapture model_marathon_convergence
```

Knobs: `MARATHON_OPS` (default 300), `MARATHON_ACTORS` (default 6). Composes
with the headless build above, so the soak runs on any headless Linux box.
MLS keygen uses the OS RNG and is not seedable — on failure the printed **op
sequence** is the repro of record, not a proptest seed.

**Nightly CI (#452 M3).** `.github/workflows/mls-tests.yml` runs the marathon at a
meaningful size (`MARATHON_OPS=500`, `MARATHON_ACTORS=8`) on a schedule. On
failure it distils the failing op sequence out of the teed log into
`marathon-failing-op-sequence.txt` and uploads it as an artifact — the op sequence
being the only repro of record for a non-seedable run.

## Machine-checked proofs (Kani, #452)

Beyond the tests, the **pure** correctness cores of the MLS/DS state machine are
proven exhaustively with the [Kani](https://model-checking.github.io/kani/)
model checker (CBMC backend). Each proof is lifted to a side-effect-free function
(no `Vec`/`String` on the hot path, no async, no DB) so CBMC reasons over the whole
input space, and each is **paired with a deliberately-broken mutant** harness
(`#[kani::should_panic]`) that Kani must refute — a proof that never fails is
worthless, so the mutant guarantees the property has teeth. Proven functions:

| Property | Function | File |
|---|---|---|
| **Watermark advance** never skips an un-handled envelope; is monotone (P2) and live (P3) | `advance_to` / `EnvelopeKind` | `messages/watermark.rs` |
| **Gap classification** never applies a commit across a gap (a missing bridging commit while a higher epoch is present → recover, never replay) | `classify` | `mls/invariants.rs` |
| **Own-commit resolution** — adopt IFF the log's bytes at this epoch are byte-for-byte ours (no phantom epoch/fork; never discard a landed own commit → no wedge); the #411 core | adopt/rollback decision | `mls/invariants.rs` |
| **Revoked/removed-device gate** — the only input that admits an external-join rebuild is `(registered ∧ member)`; a revoked or removed device can never climb back (fuzzer-finding #2) | `may_rejoin` conjunction | `mls/invariants.rs` |
| **DS head arithmetic + accept decision** — head never underflows/wraps; at any head exactly one epoch is accepted (no fork), stale/forward submits rejected (I1) | `head_epoch_of` / `accepts` | `pollis-delivery/src/commit.rs` |
| **Retention floor** (I4) — floor is non-negative (P1); Tier 1 never prunes past the slowest current member except the documented Tier-2 cap (P2 = code-level `NoLossForCurrentMember`); an unreported roster disables Tier 1 (P3) | `prune_floor` | `pollis-delivery/src/commit.rs` |

The proofs are the pure *model of record*: e.g. `accepts` is NOT wired into the
real `submit_commit` (the race-free decision must stay inside the single
conditional INSERT), it is proved alongside as the specification the SQL
implements.

## TLA+ design models (#481)

Kani proves the *code* (pure fns) correct; TLA+ proves the *design* (the abstract
state machine) correct — the complement. The specs live in `specs/tla/` and are
checked exhaustively by TLC over a small configuration (a JRE + `tla2tools.jar`;
`scripts/tlc-check.sh` fetches the pinned jar). They read as math a third party
can re-check with the public TLA+ tools.

| Spec | Models | Invariants | Status |
|---|---|---|---|
| `Delivery.tla` (**Spec B**, I3+I4) | the delivery-watermark + retention-floor machine: `Advance` abstracts `next_watermark`, `GC` the retention floor | `NoLossForCurrentMember` (retention never drops a message a current member-device still needs), `CursorMonotone`, `AcceptedLossesOnly` (the two accepted losses, nothing weaker) | ✅ authored + TLC-checked |
| `CommitLog.tla` (**Spec A**, I1/I2) | the DS `submit_commit` epoch machine under N-client concurrency: `Submit` (conditional-insert-at-head), `Apply`, `ExternalJoin`, plus the #454 P4 suite-generation migration (`MigratePrepare`/`MigrateCommit`/`MigrateAbandon`, `AdoptGeneration`) | `OnePerEpoch ∧ Gapless ∧ LineageMonotone ∧ OpeningClosesTheHead ∧ NoForeignAdopt` | ✅ authored + TLC-checked |

Each spec ships with **teeth** configs that must FAIL, so a spec can't rot into a
vacuous pass: `DeliveryBroken.cfg` guards the floor by the *fastest* member
instead of the slowest → a concrete `NoLossForCurrentMember` counterexample;
`CommitLogBroken.cfg` drops the conditional-insert guard → an `OnePerEpoch` fork;
`CommitLogMigrateBroken.cfg` drops the migration compare-and-swap → an
`OpeningClosesTheHead` counterexample where a racing commit is orphaned on the
lineage a migration is retiring. The `.github/workflows/tla.yml` gate runs both the sound pass and
the teeth-refutation on every PR touching the spec surface. Spec B was authored
**before** the #539 retention-floor code, per the "model the floor before you
ship it" rule.

## Track-B fuzzing (`fuzz/`, #481)

The same load-bearing pure fns the Kani harnesses prove also carry `cargo-fuzz`
targets in the `fuzz/` crate — continuous, coverage-guided sampling that
complements Kani's bounded exhaustive proof and makes the fns OSS-Fuzz-eligible
(they're seedable; a seed `corpus/` ships per target). One target per fn
(`next_watermark`, `classify`, `resolve`, `may_rejoin`, `prune_floor`), each
asserting the SAME property its Kani harness proves (P1/P2/P3
no-skip·monotone·liveness; no-gap-apply; no-foreign-adopt / no-own-rollback; the
recovery-gate biconditional; the retention floor's non-negative · no-loss ·
unreported-disables-Tier-1) — a violation is a fuzzer crash.

**Detached on purpose.** `cargo-fuzz` needs nightly, but this repo pins Rust
1.96.0 stable for reproducible release builds, so `fuzz/` has its own
`[workspace]` and sits in the root `Cargo.toml`'s `workspace.exclude` — it never
affects `cargo build` or the release path, and runs out of band (OSS-Fuzz-style).
`scripts/fuzz-check.sh` builds + short-runs every target on nightly;
`FUZZ_MUTANT=1 scripts/fuzz-check.sh` builds each target's `--cfg fuzz_mutant`
variant and asserts the fuzzer trips it fast (teeth — a mutant that doesn't crash
is itself a failure).

Honest scope + roadmap: `docs/machine-checked-correctness-design.md`.

## Behaviors the scenarios exercise

- **`edit_message_across_membership_changes`** covers edits across add and remove. Worth knowing when reading the assertions: `get_channel_messages` applies edit envelopes with `UPDATE message SET content = ?` only — if the recipient has no local row for the edited message (e.g. they joined after the original was sent), the edit does not populate a new row for them. The scenario asserts convergence on members that had the original cached; for late joiners it asserts only that stale plaintext never leaks.
- **`envelope_cleanup_is_watermark_gated_and_never_ttl_gated`** proves the `message_envelope` cleanup gate in `get_channel_messages` is the all-member-devices-caught-up watermark and *only* that. Its third leg is the **F3 regression test**: an envelope backdated 400 days while a member device has not collected it must SURVIVE (the deleted 30-day TTL arm destroyed such rows outright — see [database.md](./database.md#conversation_watermark)), and is reclaimed only once that device catches up. It uses two free functions in the tests file (`backdate_envelopes`, `clear_watermarks`) that poke the shared remote DB directly via `harness::writable_remote()` — the DS's OWN handle, since #987 removed the client's (`TestClient.state.remote_db` no longer exists). That is the cleanest way to construct states (old envelopes, missing watermark rows) that can't be produced by production commands alone, and it stands in for server-side effects (envelope GC), never for a client write. Do not add Tauri commands just to enable these manipulations.
- Server-side, the same invariant is pinned twice in `pollis-delivery` without needing Turso: the `messages::gc_sql_tests` unit module drives the raw `CLEANUP_*` SQL, and `tests/envelope_retention.rs` (Part E) drives `apply_envelope_gc`. Both encode "an envelope a member device has not collected survives regardless of age", plus the conservative empty-roster / never-reported edges and the positive leg proving GC still deletes once everyone has collected.
- **`messages::roster_parity_tests`** (`pollis-delivery`, #722) guards the *other* half of that invariant: the member-device roster is implemented three times — inside `CLEANUP_CHANNEL_ENVELOPES`, inside `CLEANUP_DM_ENVELOPES`, and in `commit::current_member_devices` — and they must resolve the same set (I5). Because a DELETE's roster cannot be read back, the join chain is factored into a macro that the DELETE `concat!`s in and the test SELECTs from, so there is exactly one copy of the roster SQL in the crate rather than a hand-written "equivalent" that could drift unnoticed; `the_extraction_did_not_change_the_cleanup_sql` pins the executed statements byte-for-byte, and `the_roster_is_what_the_delete_actually_gates_on` shows the DELETE's outcome pivots on exactly the devices the fragment reports. The fixture covers a plain single-device member, a member with a revoked device alongside live ones, an all-revoked member, a member device with no watermark row, a non-member (with a stray watermark row), a sibling conversation, and both the channel and DM shapes. Expected rosters are also spelled out literally, so a change applied symmetrically to both implementations still fails instead of silently redefining the rule.
- **DS tests run against the SHIPPED schema** (#875). `pollis-delivery` does not depend on `pollis-core`, and until #875 it paid for that with 22 hand-rolled `const SCHEMA` blocks (111 `CREATE TABLE` statements, 27 tables, 60 distinct definitions of them — 7 different `message_envelope`s, 6 different `group_member`s, 4 different `user_device`s). They had drifted past cosmetics: `tests/retention.rs` and `tests/envelope_retention.rs` declared `group_member` with **no primary key**, so the fixture could hold the same member twice and the Tier-1 floor's "MIN over member devices" was proved over a roster production forbids; `tests/key_packages.rs` keyed `user_device` on `(user_id, device_id)` where the baseline keys it on `device_id` alone; `tests/auth.rs` gave `mls_key_package` a `claimed_at` column that does not exist (the shipped column is `claimed INTEGER`); `tests/reset_session.rs` gave `mls_key_package` and `mls_welcome` INTEGER `id` primary keys where both ship as TEXT, and `dm_channel_member` an `accepted` column (the shipped one is `accepted_at`). Every fixture now calls `pollis_schema::apply::{main_db, log_db, single_db}`, and `tests/schema_is_not_reinvented.rs` fails the build if any DS source grows a `CREATE TABLE` again. Two things this immediately caught: `serialize.rs::welcome_failure_rolls_back_commit_and_group_info` was rolling back because its hand-rolled replacement `mls_welcome` had no `generation` column, not because of the poison CHECK it claims to inject (it now installs a trigger instead of replacing the table); and `attachment_refs.rs` seeded envelopes with `INSERT OR IGNORE INTO message_envelope (id)`, which against the real NOT NULL columns inserts nothing at all. `retention.rs`'s `a_duplicate_membership_row_is_rejected` / `a_device_id_cannot_belong_to_two_users` / `a_device_cannot_report_two_high_waters_for_one_conversation` pin the constraints the old fixtures lacked.
  - Note for fixture authors: libsql's LOCAL backend enables `PRAGMA foreign_keys` by default, while production (Turso) does not. Every `Db` now stamps its per-connection PRAGMAs on *every* checkout (#919, `db.rs` `PerConnPragmas`), and as of the teardown work the local pragma set (`LOCAL_PRAGMAS`) says `foreign_keys=OFF` — as did `SHARED_LOCAL_PRAGMAS` and `RemoteDb::conn`, both deleted in #987 when the client stopped having a database handle at all. So a fixture inherits production semantics by default and must **seed the parent rows**; it can no longer lean on a cascade that no deploy has. `flows` used to be the exception (FKs ON), which is precisely why it never caught account/group deletion leaving rows behind — see `pollis-delivery/src/teardown.rs`.
  - `tests/teardown.rs` is the enumerated deletion suite: it reads the live schema and fails if any table naming a user or a conversation is neither purged nor carried in a documented exemption list, so a new migration cannot silently add a table that account deletion forgets.
- **`tests/conversation_namespace.rs`** (`pollis-delivery`, #880) pins "one id names exactly one kind of conversation" and is deliberately split into two classes of test, because the two are proved by different things. **Guard tests** assert the clean `Forbidden` that `conversation_id_taken` produces — remove the guard and exactly these fail. **Registry tests** assert only that *no row was written*, which holds whether the guard refused the write or the `conversation` primary key did; they are what makes the invariant a property of the schema rather than of anyone's memory, and they still pass with the guard deleted. Two of them can only be proved this way: an id whose conversation no longer exists (the registry is append-only, so the three-table guard query cannot see it) and one request naming the same id twice (nothing exists yet, so no lookup can catch it). The backfill half stamps a database at the PREVIOUS schema version, seeds the row shapes production actually has (a group with channels, a group with none, a DM), runs the real migration SQL out of `POST_BASELINE_MIGRATIONS` rather than a paraphrase, and asserts exactly one registry row per conversation — including the pre-existing-collision case, where the migration must not fail and the id must go to whichever conversation had it first.
- **`tests/conn_pool.rs`** (`pollis-delivery`, #777) is the only DS test that runs the **remote** (hrana/HTTP) path — every other one uses `Db::connect_local` and never speaks hrana at all, which is why the per-request-`hyper::Client` defect went unnoticed and why a connection-reuse change could not be landed on faith. It stands up a hrana-speaking test double (an axum `/v3/pipeline` route built on the `libsql-hrana` wire types, tracking batons → streams and per-stream autocommit so `transaction()` behaves) and points `Db::connect_remote` at it. Because each libsql `Connection` owns its own `hyper::Client` and therefore its own TCP pool, *distinct client socket addresses* is an exact count of connections built — i.e. of TLS handshakes in prod. Asserted: 20 sequential requests use ONE connection (was 20); 8 concurrent holders get 8 distinct connections and 8 distinct hrana streams, then release them for reuse (exclusive checkout — the pool can never hand one connection to two callers); a connection parked past the idle window is discarded rather than reused (so a server-expired stream can't surface as a failed request later); and 6 concurrent transactions each own a stream carrying exactly their `BEGIN`/`INSERT`/`COMMIT` and nothing else. The first three fail against the old per-request-connect code; the last one guards the property reuse must not break.
- **`messages::admin_delete_visibility_tests`** (`pollis-delivery`, #693/#661 WS1) isolates the flaky `sealed_admin_delete_of_other_member_works` and RULES OUT candidate 1 (a read-after-write / `sent_at`-ordering visibility gap between the DS write and the recipient read). Driving the real DS `apply_send_message` / `apply_advance_watermark` / `apply_delete_message` across TWO connections of one shared libsql `Database` (the faithful model of the flows harness, whose clients read through the in-process DS over one shared handle), it proves the admin tombstone is ALWAYS written and ALWAYS returned by the recipient's next `sent_at > watermark` fetch — across every client `sent_at` fraction width (whole-second included) and a clock-ahead stamp. `read_your_writes_holds_across_connections_of_one_shared_database` pins the underlying primitive. Conclusion: #661's residual flake is NOT delivery — it lives in the client's *application* of the fetched tombstone (candidate 2). The flows test itself now carries a read-only `diagnose_admin_delete_failure` that, on a live failure, classifies it as tombstone-never-written / never-fetched / fetched-but-not-applied.
- **`admin_delete_redacts_the_authors_own_copy`** (#790, flows harness — `src-tauri/tests/flows/messages.rs`, not the `pollis-delivery` `messages` module its neighbours refer to) closes the last hole in the delete matrix: every other delete test re-reads a THIRD PARTY, so nothing pinned what the message's own author sees after somebody else removes it — the case a real user notices first, since their post is gone for everyone else and they are the only one still looking at it. It is a distinct path from a self-delete: the author neither initiates the delete nor authors the tombstone, so they must apply a redaction authored by someone else to a row they wrote themselves. That is the same *application* half that #693/#661 identified as the defect-prone one (delivery was ruled out), which is why it gets its own test rather than another assertion bolted onto the flaky `sealed_admin_delete_of_other_member_works`. Proven non-vacuous by mutation: making the ingest redaction `UPDATE` skip rows the applying device authored fails it with the author still holding plaintext.
- **`watermark::delete_consumption_tests`** + **`ingest::delete_resolution_tests`** (`pollis-core`, #693) pin candidate 2 AND its fix. The asymmetry: an undecryptable `message` is held back and re-fetched to success, whereas a `delete` tombstone used to be `is_handled == true` UNCONDITIONALLY — `ingest.rs` classified EVERY tombstone as handled regardless of whether its fire-and-forget redaction `UPDATE` landed, so a redaction matching zero rows or hitting a transient local-DB error was consumed-and-lost, never retried (the deleted message stayed readable forever). The fix: ingest now resolves each tombstone's fate from the redaction's OUTCOME (`resolve_delete`) and re-classifies a not-yet-applied-but-still-applicable one to `EnvKind::DeletePending` (`is_handled == false`), so the watermark stops strictly below it and the next fetch re-applies it — mirroring the undecryptable-message retry path. `delete_resolution_tests` pins the edge matrix: applied / already-redacted / transient-error(retry) / target-absent-but-awaiting-ingest(retry) / target-absent-pre-join(terminal, does not spin). `delete_consumption_tests` pins the watermark half (an applied `Delete` is consumed; a `DeletePending` is retried). The DS cursor/delivery is not the culprit (proven by `admin_delete_visibility_tests`); the drop was an *application* failure, now closed.
- **`dm_multi_device_round_trip`** drives the device-enrollment command chain via the `enroll_second_device` helper and proves the MLS tree in a DM expands to every enrolled device of every member. Alice and Bob each run two devices; the DM's `dm_channel_member` row is still keyed per user, but reconcile populates one MLS leaf per device, so a message sent from any of the four devices decrypts on the other three. A non-member (carol) cannot decrypt any of the messages.
- **`dm_invite_reject_removes_from_tree`** covers the reject path: bob runs two devices, both are in the MLS tree at create time, bob_d1 calls `leave_dm_channel` on the pending request, and reconcile removes BOTH bob devices' leaves. The `dm_channel_member` row (keyed per user) goes to zero, and neither bob device may decrypt a subsequent alice-authored message; the DM also disappears from both bob devices' `list_dm_channels` / `list_dm_requests`.
- **`dm_re_invite_after_reject`** proves that after a reject, alice can open a fresh DM with bob (new `dm_channel.id`), bob accepts on one device, and both bob devices re-enter the MLS tree — alice→bob and bob→alice round-trips both decrypt. Alice's side may still hold a ghost row for the rejected DM (leave_dm_channel only tears the channel down when zero members remain), but bob's side shows no trace, and the rejected id never carries fresh-DM plaintext.
- **`user_block_lifecycle`** walks block → unblock across a DM: pre-block `search_user_by_username` resolves; `block_user` inserts a `user_block` row and hides the DM from the blocker's `list_dm_channels`/`list_dm_requests` but NOT from the blocked side (dm.rs:169-173 privacy property); `create_dm_channel` fails with BLOCK_ERR while the block is active; `unblock_user` drops the row and the DM resurfaces on alice's side; a subsequent `create_dm_channel` to a fresh peer succeeds.

## Known flakes

- **`channel_message_round_trip`** can fail with "stream not found" during `send_group_invite`'s MLS reconcile — a libsql hrana stream timeout. It passes reliably in isolation and in the current suite, but is sensitive to connection state accumulation. If it flakes again, the first suspect is stream lifetime, not test logic.

### Rules the fixed flakes left behind

- **Never assert an exact count over a real dial/round trip.** `net::overlay`'s latency-ordering tests seeded latencies and then asserted "6 of 6 dials took the fast relay" *through real loopback QUIC connections* — but a successful dial folds its own elapsed time into the latency EWMA and a failed one marks the endpoint dead, so on a loaded runner the pool legitimately re-ordered itself and the test went red with nothing broken (#953, and #812/#813 before it). They now inject a `PathDialer` (`RealRelayFactory::with_dialer`) that records which endpoint each `connect` chose and dials nothing, so the ordering is asserted exactly and the clock is out of the loop. If a selection test needs a socket, assert the property, not the tally.
- **No process-global mutation from a rig with live threads** — see the data-dir section above.

## WebDriver E2E tests (`e2e/`)

The only tier that drives the **actual shipped app** — the real WebKitGTK
WebView inside the Tauri shell, real Rust core, real Tauri IPC — rather than
`MockRuntime` or a browser build of the frontend. **Thirteen scenario scripts**
under `e2e/` (plus `run.js`, the single entry point), sharing
`e2e/lib/harness.js` for the `tauri-driver`/WebKitWebDriver plumbing
(raw `webdriverio` `remote()` calls, not the wdio test runner — the runner
intermittently stalls the first WebView command against this webkit2gtk
build):

| script | proves | needs delivery service / Turso? |
|---|---|---|
| `smoke.js` | app launches, login screen renders | no |
| `e2e.js` | full signup: email → OTP → secret key → PIN → app-ready | yes (writable test Turso) |
| `invalid-otp.js` | wrong OTP code is rejected, doesn't advance past code entry | yes (writable test Turso) |
| `restart-persistence.js` | sign up, kill the app, relaunch on the same data dir → session and data survive | yes |
| `two-client.js` | DM request → accept → message, across two real app instances | yes |
| `two-client-dm-reply.js` | the reverse direction: B replies, A sees it | yes |
| `two-client-channel.js` | group + channel convergence: create, invite, accept, post, receive | yes |
| `two-client-delete.js` | delete convergence — the peer sees the tombstone | yes |
| `two-client-call.js` | 1:1 call place + accept, both sides see each other | yes (+ LiveKit) |
| `two-client-camera.js` | camera on → the peer renders the remote tile | yes (+ LiveKit) |
| `two-client-screenshare.js` | screenshare → the peer renders the remote tile | yes (+ LiveKit) |
| `two-client-voice-channel.js` | voice channel join/leave, participant counts converge | yes (+ LiveKit) |
| `voice-channel-no-mic.js` | joining a voice channel with no microphone degrades instead of failing | yes (+ LiveKit) |

Scenario coverage against the full intended matrix — including what is **not** yet
automated — is tracked in [`e2e/SCENARIOS.md`](../../e2e/SCENARIOS.md), which ranks
candidates by value × (1 − confidence). Each two-client scenario also has its own
dispatch-only workflow (`.github/workflows/e2e-*.yml`); they are deliberately not
merge-blocking, since they need real LiveKit and a writable Turso.

Every scenario runs through one entry point — `pnpm e2e <scenario>` (one
package.json script, not one per test; the scenario list is the `e2e/*.js`
files on disk). `pnpm e2e` with no argument lists them.

```bash
pnpm --filter @pollis/e2e e2e smoke        # fast, no external deps
pnpm --filter @pollis/e2e e2e e2e          # full signup flow
pnpm --filter @pollis/e2e e2e invalid-otp
```

`smoke.js` is the fast, backend-free one: the logged-out path
(`checkStoredSession()` in `frontend/src/App.tsx`) resolves entirely from
local Tauri commands, so it never calls out to the delivery service or
Turso. `.github/workflows/e2e-smoke.yml` runs it on `workflow_dispatch`. Every
other scenario needs a writable test Turso with the schema applied plus a running
delivery service — all stood up automatically by
`e2e/scripts/start-backend.sh` (a libsql server + `scripts/db-apply.sh` +
the real `pollis-delivery` binary; issue #570, M1). `.github/workflows/e2e-full.yml`
runs them on `workflow_dispatch` behind that script; locally,
`eval "$(e2e/scripts/start-backend.sh)"` then run the scripts (see
`e2e/README.md`).

Full details — how the stack is stood up, `.env.test` schema bootstrap,
`data-testid` conventions, and the WebKitWebDriver quirks (native `.click()`
doesn't fire React handlers, IPv6-only Vite loopback, orphan-process
reaping) — live in `e2e/README.md`, not duplicated here.

## Playwright UI specs (`e2e/*.spec.js`)

For behaviour that lives entirely in the renderer — composer interaction,
per-skin branching, how a message body renders — the native stack costs minutes
and proves nothing extra. These specs drive the **browser** build with
`VITE_PLAYWRIGHT=true`, which vite-aliases `@tauri-apps/api/*` to
`frontend/src/__mocks__/`. Real React tree, real CSS tokens, real `useSkin()`
branching; no MLS, no delivery service, no Turso.

```bash
pnpm --filter @pollis/e2e exec playwright install chromium   # once
pnpm --filter @pollis/e2e e2e:ui                             # all specs
```

| spec | what it proves |
|---|---|
| `mentions.spec.js` | `@username` autocomplete + rendering in BOTH skins (#843) |
| `bookmarks.spec.ts` | Saved messages + permalinks, and what an unresolvable permalink refuses to say, in BOTH skins (#854) |
| `emoji.spec.ts` | The custom-emoji picker (full standard set, search, caret-accurate insertion) and `<:name:hash>` rendering, in BOTH skins (#848) |
| `invite-links.spec.ts` | Invite-link create / one-time copy / revoke, in BOTH skins (#847) |
| `voice-controls.spec.ts` | Push-to-talk, deafen and the input-mode toggle — the four mic states drawn distinctly — in BOTH skins (#849) |
| `autolock.spec.ts` | Idle auto-lock: the window is chosen, reaches the backend and survives a restart; the shell reports activity; a backend lock drops to the PIN gate **and empties the query cache**, in BOTH skins (#851) |
| `i18n.spec.ts` | The language selector, switching, per-device persistence, OS-locale default and the English fallback — driven through a synthetic locale so it survives the real language list changing, in BOTH skins (#855). Also pins that the sidebar's settings rows are reached by `sidebar-row-*` testid and not by their translated label, under a locale that renames every one of them (#932) |
| `ipc-efficiency.spec.ts` | IPC/query-layer COUNTS (#874): one batched preview call per list instead of one per row, zero refetches on window focus, a closed Cmd+K panel costing zero member queries, `membership_changed` touching only the named group's roster, join requests keyed per group id, own-profile vs public-profile not colliding. Skin-agnostic except the two tests that also assert something renders |
| `rtl.spec.ts` | Right-to-left layout, asserted as **measured geometry** (`getBoundingClientRect`, a `Range` over the text, the painted physical border edge) rather than a `dir` attribute — which passes on unmirrored code. Drives the real `ar` locale and pins the LTR case in the same body, in BOTH skins (#855) |

| `receipts.spec.ts` | DM delivery/read indicators in BOTH skins — delivered vs read visually distinct, none in group channels, per-reader fractions in group DMs (#857) |
| `render-cost.spec.ts` | Regression guards on message-log render cost in BOTH skins — typing in the edit bar, opening the reply bar and arrow-key navigation must re-render **zero** rows; a shell re-render must not re-render the sidebar; paired with the other half (skin flip restructures rows, an edit updates its row, day dividers survive) so a memo cannot pass by freezing the UI (#874) |
| `message-window.spec.ts` | The virtualised log: only the visible slice is in the DOM, and every DOM-locating path still reaches a row outside the window, in BOTH skins (#874). Plus the load-more seam (#934) — a `MutationObserver` proves some DOM batch carries the prepended rows while the log still reports fetching, which a poll could never catch |
| `linkify.spec.ts` | URL detection in message bodies in BOTH skins — every body keeps the link it *starts* with, the media unfurl agrees with the linkifier about which URLs exist, and a bare `www.` link gets a protocol. Guards the pattern shared by `LinkifiedText` and `MediaLinkUnfurl` (#874) |
| `thread-panel.spec.ts` | The thread panel's timestamps in BOTH skins — a seconds-precision `created_at` must render the real date rather than 1970, a millisecond one must be left alone, and the thread must agree with the channel about when a message was sent (#874) |

`.spec.ts` is as welcome as `.spec.js`; one config matches both.

Five things to know before writing one:

- **Fixtures go in `window.__POLLIS_PRELOAD__`** via `page.addInitScript`, read
  once by the mock on load. Extend `MockStore` in `frontend/src/__mocks__/tauri-core.ts`
  to add a new fixture slice.
- **Navigate through the UI, not the URL.** The router uses
  `createMemoryHistory` (`frontend/src/router.tsx`), so `page.goto('/groups/…')`
  changes the address bar and renders Root anyway. Click
  `menu-item-groups` → `group-option-<id>` → `channel-option-<id>`.
- **A fixture slice nobody seeds is a coverage hole, not a saving.** `read_thread_messages`
  and `list_thread_summaries` returned a hard-coded `[]` for as long as threads have
  existed, so no spec ever rendered a thread row — and a timestamp bug in it survived a
  wholesale rewrite of that component (#874). They read `threadMessages` (keyed by root
  message id) and `threadSummaries` (keyed by conversation id) now.
- **The mock is a real surface — keep it current.** An unhandled command returns
  `null`, and `null` is not `undefined`, so `const { data = [] }` defaults do NOT
  catch it and the page dies in an error boundary. When a Rust command is renamed
  (`get_channel_messages` → `read_channel_messages`) the mock silently rots until
  someone runs these specs.
- **Invoke counts are assertable (#874).** The mock tallies every command in
  `window.__tauriInvokeCounts` and exposes `window.__resetTauriInvokeCounts()` to
  scope a count to one interaction. That is what makes an IPC-efficiency claim a
  number rather than an impression — `ipc-efficiency.spec.ts` pins how many calls
  a screen or a focus round is allowed. Counting lives in the shared mock, not in
  a per-spec wrapper, because every spec imports the same module.
- **Realtime events are drivable too (#874).** Every `Channel` the app opens is
  registered, and `window.__emitRealtimeEvent(event)` pushes one through the live
  subscription (the most recently created channel — StrictMode double-invokes
  effects, so older ones are dead). Once `refetchOnWindowFocus` is off, realtime
  IS the freshness mechanism, so "this event invalidates exactly these caches"
  has to be testable.
- **Backend-pushed events are drivable.** `frontend/src/__mocks__/tauri-event.ts`
  keeps a listener registry and exposes `window.__tauriEmit(name, payload)`, so a
  spec can stand in for the shell's `AppHandle::emit` (`autolock.spec.ts` fires
  `auto-lock` this way). It used to drop every handler on the floor, which made
  that whole class of behaviour untestable in this tier. `bridge/invoke.ts`
  unwraps `e.payload`, so the registry delivers `{ payload }`, as real Tauri does.
- **Render cost is measurable here.** `frontend/src/utils/renderProbe.ts` exposes
  `probeRender(name)`, called at the top of a component body; under
  `VITE_PLAYWRIGHT=true` it counts render-function entries into
  `window.__pollisRenders`. A `React.memo` bail-out means the body never runs, so
  the counter is exactly the signal — no stopwatch, no flake. It compiles to
  nothing in a real build (`import.meta.env.VITE_PLAYWRIGHT` is statically
  replaced, so Rollup drops the body).
  Two rules when asserting on it: counts are **relative only** — StrictMode
  double-invokes render in dev, so absolute numbers are 2x and must never be
  hard-coded — and every "must not re-render" assertion needs a companion "must
  still re-render" case, or a component that has stopped updating entirely will
  sail through. Assert the probe is non-zero first, or the whole file can pass
  vacuously against a counter that was never wired up.
