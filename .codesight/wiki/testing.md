# Testing

Pollis has three tiers of automated tests:

1. **Unit tests** (in-crate `#[cfg(test)]` modules) — pure logic, in-memory rusqlite schemas, no I/O.
2. **Integration harness** (`src-tauri/tests/flows.rs`) — drives real Tauri commands end-to-end against a disposable test Turso database. This document covers the harness.
3. **Playwright E2E** (`pnpm test:e2e`) — browser-level tests of the frontend.

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

## `.env.test`

Tests require a disposable Turso database. Create `.env.test` at the repo root:

```
TURSO_URL=libsql://pollis-test-<yours>.turso.io
TURSO_TOKEN=<read/write token>
```

The harness wipes all tables at the start of each test (see `wipe_remote` in `src-tauri/src/test_harness.rs`), so never point this at a production or shared-dev DB.

Tests serialize on a process-wide mutex (`serial_test`) because the wipe would race otherwise.

## Architecture

### `TestWorld`

Lazy, process-wide singleton (`tokio::sync::OnceCell`) that owns:

- `Arc<RemoteDb>` — one libsql connection pool shared across all clients in the process.
- `Config` — test config loaded from `.env.test`.
- A tempdir used for per-user SQLCipher files, exposed via `POLLIS_DATA_DIR`.

### `TestClient`

One simulated device for one user. Owns:

- `App<MockRuntime>` + `WebviewWindow<MockRuntime>` — the headless Tauri runtime.
- `Arc<AppState>` — wired with an `InMemoryKeystore` and the shared `RemoteDb`.
- `profile: Option<UserProfile>` — populated after `sign_up`.

### `invoke<T>`

Wraps `tauri::test::get_ipc_response` in `spawn_blocking` (it uses `std::sync::mpsc` internally so it cannot run on the async runtime directly). Takes a webview, command name, and JSON args; returns the command's `Result<T, String>`.

### Schema bootstrap

The harness embeds `remote_schema.sql` + every numbered migration via `include_str!` and applies them idempotently at first use (tracked via the `schema_migrations` table). There is no dependency on `pnpm db:push` — a fresh Turso database works out of the box.

Known prod schema drift is applied in `apply_drift_fixups` (currently: `group_invite.status` column). Keep this list short; drift should be fixed in a real migration, not codified here.

## DEV_OTP

The harness sets `DEV_OTP=000000` before spinning up any client. `verify_otp` short-circuits email send and accepts this fixed code when `debug_assertions` are on — which they are in integration tests. No real emails are sent and no OTP storage round-trips through Resend.

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

## Extending the harness

- **Add a helper to `TestClient`** when a command is used from more than one scenario, so assertions stay readable.
- **Register new commands in `build_client_app`.** The `tauri::generate_handler![...]` macro call in `src-tauri/src/test_harness.rs` must include every command invoked by a test.
- **Add FK-safe wipes.** If you introduce a new table referenced by tests, add it to `wipe_remote` in the correct order (child tables before parent).

## Behaviors the scenarios exercise

- **`edit_message_across_membership_changes`** covers edits across add and remove. Worth knowing when reading the assertions: `get_channel_messages` applies edit envelopes with `UPDATE message SET content = ?` only — if the recipient has no local row for the edited message (e.g. they joined after the original was sent), the edit does not populate a new row for them. The scenario asserts convergence on members that had the original cached; for late joiners it asserts only that stale plaintext never leaks.

## Known flakes

- **`channel_message_round_trip`** can fail with "stream not found" during `send_group_invite`'s MLS reconcile — a libsql hrana stream timeout. It passes reliably in isolation and in the current suite, but is sensitive to connection state accumulation. If it flakes again, the first suspect is stream lifetime, not test logic.
