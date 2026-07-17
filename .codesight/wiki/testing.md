# Testing

Pollis has three tiers of automated tests:

1. **Unit tests** (in-crate `#[cfg(test)]` modules) — pure logic, in-memory rusqlite schemas, no I/O.
2. **Integration harness** (`src-tauri/tests/flows.rs`) — drives the real `pollis-core` commands end-to-end against a disposable test Turso database. Most of this document covers the harness.
3. **WebDriver E2E tests** (`e2e/`) — drives the real shipped Tauri app (native WebKitGTK WebView, real Rust core) via `tauri-driver`. See [WebDriver E2E tests](#webdriver-e2e-tests-e2e) below.

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
use the file-backed keystore automatically).

### pollis-tui in CI (#487)

`.github/workflows/mls-tests.yml` also runs `cargo test -p pollis-tui` — the
terminal client's unit tests plus its headless in-process-DS **smoke rig**. The
TUI drives `pollis-core`'s sync path directly (no Tauri/IPC), so a change to that
path could regress it unnoticed; gating it in CI catches that. The smoke rig
applies the same `POST_BASELINE_LOG_MIGRATIONS` (the post-baseline commit-log-DB
migrations) as the app, so it exercises the current log-DB schema rather than a
stale baseline. See [pollis-tui.md](./pollis-tui.md).

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
| `CommitLog.tla` (**Spec A**, I1/I2) | the DS `submit_commit` epoch machine under N-client concurrency: `Submit` (conditional-insert-at-head), `Apply`, `ExternalJoin` | `OnePerEpoch ∧ Gapless ∧ HeadMonotone ∧ NoForeignAdopt` | ✅ authored + TLC-checked |

Each spec ships with a **teeth** config that must FAIL (e.g. `DeliveryBroken.cfg`
guards the floor by the *fastest* member instead of the slowest → TLC produces a
concrete `NoLossForCurrentMember` counterexample), so a spec can't rot into a
vacuous pass. The `.github/workflows/tla.yml` gate runs both the sound pass and
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
- **`envelope_cleanup_ttl_or_watermark`** proves each leg of the `message_envelope` cleanup gate in `get_channel_messages`: the 30-day TTL and the all-members-caught-up watermark. It uses two free functions in the tests file (`backdate_envelopes`, `clear_watermarks`) that poke the shared remote DB directly via `TestClient.state.remote_db` — that's the cleanest way to construct states (old envelopes, missing watermark rows) that can't be produced by production commands alone. Do not add Tauri commands just to enable these manipulations.
- **`dm_multi_device_round_trip`** drives the device-enrollment command chain via the `enroll_second_device` helper and proves the MLS tree in a DM expands to every enrolled device of every member. Alice and Bob each run two devices; the DM's `dm_channel_member` row is still keyed per user, but reconcile populates one MLS leaf per device, so a message sent from any of the four devices decrypts on the other three. A non-member (carol) cannot decrypt any of the messages.
- **`dm_invite_reject_removes_from_tree`** covers the reject path: bob runs two devices, both are in the MLS tree at create time, bob_d1 calls `leave_dm_channel` on the pending request, and reconcile removes BOTH bob devices' leaves. The `dm_channel_member` row (keyed per user) goes to zero, and neither bob device may decrypt a subsequent alice-authored message; the DM also disappears from both bob devices' `list_dm_channels` / `list_dm_requests`.
- **`dm_re_invite_after_reject`** proves that after a reject, alice can open a fresh DM with bob (new `dm_channel.id`), bob accepts on one device, and both bob devices re-enter the MLS tree — alice→bob and bob→alice round-trips both decrypt. Alice's side may still hold a ghost row for the rejected DM (leave_dm_channel only tears the channel down when zero members remain), but bob's side shows no trace, and the rejected id never carries fresh-DM plaintext.
- **`user_block_lifecycle`** walks block → unblock across a DM: pre-block `search_user_by_username` resolves; `block_user` inserts a `user_block` row and hides the DM from the blocker's `list_dm_channels`/`list_dm_requests` but NOT from the blocked side (dm.rs:169-173 privacy property); `create_dm_channel` fails with BLOCK_ERR while the block is active; `unblock_user` drops the row and the DM resurfaces on alice's side; a subsequent `create_dm_channel` to a fresh peer succeeds.

## Known flakes

- **`channel_message_round_trip`** can fail with "stream not found" during `send_group_invite`'s MLS reconcile — a libsql hrana stream timeout. It passes reliably in isolation and in the current suite, but is sensitive to connection state accumulation. If it flakes again, the first suspect is stream lifetime, not test logic.

## WebDriver E2E tests (`e2e/`)

The only tier that drives the **actual shipped app** — the real WebKitGTK
WebView inside the Tauri shell, real Rust core, real Tauri IPC — rather than
`MockRuntime` or a browser build of the frontend. Three scripts under `e2e/`,
sharing `e2e/lib/harness.js` for the `tauri-driver`/WebKitWebDriver plumbing
(raw `webdriverio` `remote()` calls, not the wdio test runner — the runner
intermittently stalls the first WebView command against this webkit2gtk
build):

| script | proves | needs delivery service / Turso? |
|---|---|---|
| `smoke.js` | app launches, login screen renders | no |
| `e2e.js` | full signup: email → OTP → secret key → PIN → app-ready | yes (writable test Turso) |
| `invalid-otp.js` | wrong OTP code is rejected, doesn't advance past code entry | yes (writable test Turso) |

```bash
pnpm --filter @pollis/e2e smoke        # fast, no external deps
pnpm --filter @pollis/e2e test         # full signup flow
pnpm --filter @pollis/e2e invalid-otp
```

`smoke.js` is the fast, backend-free one: the logged-out path
(`checkStoredSession()` in `frontend/src/App.tsx`) resolves entirely from
local Tauri commands, so it never calls out to the delivery service or
Turso. `.github/workflows/e2e-smoke.yml` runs it on `workflow_dispatch`. The
other two need a writable test Turso with the schema applied plus a running
delivery service — all stood up automatically by
`e2e/scripts/start-backend.sh` (a libsql server + `scripts/db-apply.sh` +
the real `pollis-delivery` binary; issue #570, M1). `.github/workflows/e2e-full.yml`
runs both on `workflow_dispatch` behind that script; locally,
`eval "$(e2e/scripts/start-backend.sh)"` then run the scripts (see
`e2e/README.md`).

Full details — how the stack is stood up, `.env.test` schema bootstrap,
`data-testid` conventions, and the WebKitWebDriver quirks (native `.click()`
doesn't fire React handlers, IPv6-only Vite loopback, orphan-process
reaping) — live in `e2e/README.md`, not duplicated here.
