# Testing

Pollis has two tiers of automated tests:

1. **Unit tests** (in-crate `#[cfg(test)]` modules) — pure logic, in-memory rusqlite schemas, no I/O.
2. **Integration harness** (`src-tauri/tests/flows.rs`) — drives the real `pollis-core` commands end-to-end against a disposable test Turso database. This document covers the harness.

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

## The `DsFault` seam — injecting DS-side faults

The flows harness routes every commit submit through an **in-process
`pollis-delivery`** instance (`spawn_in_process_delivery` in `harness.rs`). That
seam is the only place a fault can be injected *without* touching production code:
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

## Behaviors the scenarios exercise

- **`edit_message_across_membership_changes`** covers edits across add and remove. Worth knowing when reading the assertions: `get_channel_messages` applies edit envelopes with `UPDATE message SET content = ?` only — if the recipient has no local row for the edited message (e.g. they joined after the original was sent), the edit does not populate a new row for them. The scenario asserts convergence on members that had the original cached; for late joiners it asserts only that stale plaintext never leaks.
- **`envelope_cleanup_ttl_or_watermark`** proves each leg of the `message_envelope` cleanup gate in `get_channel_messages`: the 30-day TTL and the all-members-caught-up watermark. It uses two free functions in the tests file (`backdate_envelopes`, `clear_watermarks`) that poke the shared remote DB directly via `TestClient.state.remote_db` — that's the cleanest way to construct states (old envelopes, missing watermark rows) that can't be produced by production commands alone. Do not add Tauri commands just to enable these manipulations.
- **`dm_multi_device_round_trip`** drives the device-enrollment command chain via the `enroll_second_device` helper and proves the MLS tree in a DM expands to every enrolled device of every member. Alice and Bob each run two devices; the DM's `dm_channel_member` row is still keyed per user, but reconcile populates one MLS leaf per device, so a message sent from any of the four devices decrypts on the other three. A non-member (carol) cannot decrypt any of the messages.
- **`dm_invite_reject_removes_from_tree`** covers the reject path: bob runs two devices, both are in the MLS tree at create time, bob_d1 calls `leave_dm_channel` on the pending request, and reconcile removes BOTH bob devices' leaves. The `dm_channel_member` row (keyed per user) goes to zero, and neither bob device may decrypt a subsequent alice-authored message; the DM also disappears from both bob devices' `list_dm_channels` / `list_dm_requests`.
- **`dm_re_invite_after_reject`** proves that after a reject, alice can open a fresh DM with bob (new `dm_channel.id`), bob accepts on one device, and both bob devices re-enter the MLS tree — alice→bob and bob→alice round-trips both decrypt. Alice's side may still hold a ghost row for the rejected DM (leave_dm_channel only tears the channel down when zero members remain), but bob's side shows no trace, and the rejected id never carries fresh-DM plaintext.
- **`user_block_lifecycle`** walks block → unblock across a DM: pre-block `search_user_by_username` resolves; `block_user` inserts a `user_block` row and hides the DM from the blocker's `list_dm_channels`/`list_dm_requests` but NOT from the blocked side (dm.rs:169-173 privacy property); `create_dm_channel` fails with BLOCK_ERR while the block is active; `unblock_user` drops the row and the DM resurfaces on alice's side; a subsequent `create_dm_channel` to a fresh peer succeeds.

## Known flakes

- **`channel_message_round_trip`** can fail with "stream not found" during `send_group_invite`'s MLS reconcile — a libsql hrana stream timeout. It passes reliably in isolation and in the current suite, but is sensitive to connection state accumulation. If it flakes again, the first suspect is stream lifetime, not test logic.
