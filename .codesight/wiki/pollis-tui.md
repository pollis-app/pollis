# Terminal Client (`pollis-tui`)

A persistent, full-screen **terminal** messaging client for Pollis, built
directly on `pollis-core` with **no Tauri, no IPC, no WebView**. It calls the
exact same `pollis_core::commands::*` surface the desktop app reaches over Tauri
`invoke`, but **in-process**. Ships as the `pollis` binary.

- **Crate:** `pollis-tui/` (workspace member). Binary name: `pollis`.
- **Design contract:** [`docs/pollis-tui-spec.md`](../../docs/pollis-tui-spec.md) — authoritative.
- **Status:** FEATURE-COMPLETE (M0–M4). M0 (skeleton) + M1 (auth) + M2 (read core)
  + M2b (three-pane UI) + **M3 write CORE** (`src/send.rs`) + **M3b compose/create
  UI** (compose+send, accept DM, create group/channel/DM, invite) + **M4 core**
  (`src/enroll.rs` — multi-device enrollment + Secret-Key recovery, gated by two
  multi-device smokes) + **M4b enrollment/recovery UI** (`src/enroll_flow.rs` +
  screens). Text messaging only — attachments/media are intentionally out of scope
  (this is an RCS-like text client). Remaining possible enhancement: realtime
  (replace the poll loop with a LiveKit inbox).

  **M4b screen flow** (spec §7): `verify_otp` returns `enrollment_required`. If
  false → first-device `SetPin` (unchanged). If true → **EnrollChoice** (↑/↓ or
  1/2): *Approval* → `request_enrollment` → **EnrollWaiting** (shows the
  verification code, polls `enrollment_status` on a tick; retry on rejected/
  expired) → on Approved, `SetPin` → `enroll::set_pin_and_finalize`; or *Recover*
  → **RecoverKey** (Secret-Key entry) → `enroll::recover` → same finalize tail.
  On an existing signed-in device, `E` opens **PendingEnrollments** (↑/↓, `a`
  approve with the shown code, `r` reject). The pure branch/selection logic lives
  in `src/enroll_flow.rs` (`PinFlow`/`EnrollChoice`/`PollOutcome`/`ApprovalState`,
  unit-tested); async work is in `app.rs`.

## Why a TUI

`pollis-core` is shell-agnostic: auth, PIN, MLS, groups/channels/DMs and messages
are plain async functions taking `&Arc<AppState>`. The React app reaches them over
Tauri `invoke`; the TUI calls them directly — *less* indirection than the desktop
app, and it builds + smoke-tests headlessly in-box (`--no-default-features`, so no
webkit/ALSA/dbus).

## Architecture

```
pollis-tui (binary `pollis`)
  ratatui + crossterm      render + input
        │
  App (UI state machine)   src/app.rs  — Screen enum + Action queue
        │  direct async calls
        ▼
  pollis_core::commands::* (&Arc<AppState>)   src/auth.rs — order-enforcing wrappers
        │
  AppState { Config, RemoteDb, log_db, file Keystore, local SQLCipher DB }
        │ reads (direct)              │ writes (via Delivery Service)
        ▼                            ▼
      Turso libSQL              pollis-delivery
```

### Source layout
| File | Role |
|---|---|
| `src/main.rs` | Entry. Multi-thread tokio runtime, `Config::from_env` → `AppState::new`, terminal setup, `tokio::select!` render/input loop (key input + UI-refresh tick), panic hook, clean sync-loop shutdown. |
| `src/terminal.rs` | `TerminalGuard` — RAII enter/leave raw mode + alt screen; restores the terminal on **every** exit path (quit, `?`, panic). |
| `src/app.rs` | `App` state machine: `Screen` enum, synchronous `on_key` (auth + Home navigation/compose/prompt), async `run(Action) -> Option<Action>` (returns a follow-up action), background `SyncLoop` ownership + `shutdown()`. M3b adds the `SendMessage`/`AcceptDm`/`SubmitPrompt` actions + the `begin_*`/`do_*` compose/create handlers. |
| `src/home.rs` | The three-pane model + **pure, unit-tested** helpers: sidebar flattening (`build_sidebar_rows`), selection movement (`step_selection`/`clamp_selection`), bottom-anchored scroll windowing (`visible_window`), scrollback prefetch (`should_load_older`), page merge/dedup (`merge_messages`). M3b adds the input-mode model (`HomeMode` = Navigate/Compose/Prompt, `PromptKind` + its `label()`), the empty-input guard (`is_blank`), and the pure selection-context helpers (`selected_dm_request`, `context_group`). |
| `src/auth.rs` | Thin wrappers over `pollis_core::commands::{auth,pin}` that encode the M1 call order. No forked logic. |
| `src/data.rs` | Typed read layer: `load_conversations` (tree) + `channel_messages`/`dm_messages` (paginated). Shared by `sync.rs` and the Home UI. |
| `src/send.rs` | **M3 write CORE.** Typed passthroughs over the exact core writes (`send_message`, `create_group`, `create_channel`, `create_dm_channel`, `accept_dm_request`, `invite_to_group`) + ergonomic shorthands with UI defaults baked in (`send_text`, `new_group`, `new_channel`, `start_dm`, `accept_dm`, `invite`). No forked logic — every write routes through the DS via the core fn. M3b calls one fn per action. |
| `src/enroll.rs` | **M4 DATA core.** Typed, order-enforcing wrappers over `pollis_core::commands::device_enrollment` for the two "add a terminal to an existing account" flows: sibling-approval enrollment (`request_enrollment`/`enrollment_status`/`finalize` on the new device, `pending_requests`/`approve`/`reject` on the existing one) and Secret-Key recovery (`recover`, plus the last-resort `reset_and_recover`). No forked logic. Re-exports `EnrollmentHandle`/`EnrollmentStatus`/`PendingEnrollmentRequest` for the M4b UI. |
| `src/sync.rs` | §6 poll loop: `sync_once`/`sync_rounds` + `spawn_loop` (cancelable background `SyncLoop`). |
| `src/ui.rs` | Pure `render(frame, &app)` — header (identity · open-conversation name · sync spinner) / three-pane Home body / auth card / status line. |

### Key design points (from the spec)
- **Multi-thread runtime is mandatory.** `pollis-core`'s DB/keystore paths use
  `spawn_blocking`; a current-thread runtime deadlocks. Hence
  `#[tokio::main(flavor = "multi_thread")]`.
- **No `media` feature anywhere.** The crate depends on `pollis-core` with
  `default-features = false`, so `media` (livekit/libwebrtc/cpal) and
  `os-keystore` (keyring/dbus) are both **off**. Voice/screenshare/camera are out
  of scope; the keystore is the file-backed JSON store.
- **Reads go direct to Turso; writes go through the Delivery Service.** This is the
  post-#419 model — `POLLIS_DELIVERY_URL` is **mandatory** config. The TUI invents
  no new backend path.
- **Own device identity.** Set `POLLIS_DATA_DIR` (default
  `~/.local/share/pollis-tui`) so the TUI's file keystore + local SQLCipher DB do
  not share identity with the desktop app — it enrolls as its own device with its
  own `device_id` and MLS leaf.
- **Input isolation.** `crossterm::event::read` is blocking, so it runs on a
  dedicated OS thread that forwards key *presses* over an mpsc channel; the tokio
  loop never blocks on stdin. Only `KeyEventKind::Press` is forwarded (some
  terminals also emit Release/Repeat, which would double keystrokes).

## The auth-order gotcha (spec §7) — READ THIS

`verify_otp` **deliberately leaves the local SQLCipher DB closed.** Until `set_pin`
(first device) or `unlock` (returning) opens it, every DB-touching command fails
with `"Not signed in"`. Order is load-bearing.

**First-device signup:**
```
request_otp(email)
verify_otp(email, code) -> UserProfile      // LEAVES LOCAL DB CLOSED
set_pin(new_pin, old_pin = None)            // OPENS the per-user SQLCipher DB
initialize_identity(user_id)                // publishes the MLS key package
```
`set_pin` must land **before** `initialize_identity` (which touches the DB). This
mirrors `TestClient::sign_up` in `src-tauri/tests/flows/harness.rs`.

**Returning launch:**
```
get_session() -> Option<UserProfile>        // rehydrate profile; also sets device_id
unlock(user_id, pin)                        // re-opens the local DB
// then the M2 sync loop
```

`src/auth.rs` wraps these in `set_pin_and_init` / `unlock` / `boot` so the order
can't be gotten wrong at a call site.

### Screen flow
```
Booting ──get_session──► Returning ─► Unlock ──unlock──► Home
                       └► Fresh ─► Email ─► Otp ─► SetPin ──set_pin+init──► Home
```

## Build & run (headless, in-box)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build  -p pollis-tui                    # media/keyring off via the crate's default-features = false
cargo clippy -p pollis-tui -- -D warnings
```
Both are clean in-box (no webkit/ALSA/dbus).

Running the binary needs live config in the environment:
```bash
export TURSO_URL=... TURSO_TOKEN=...
export POLLIS_DELIVERY_URL=https://api.pollis.com     # mandatory: all writes route here
export R2_S3_ENDPOINT= R2_ACCESS_KEY_ID= R2_SECRET_KEY= R2_PUBLIC_URL=   # unused in v1; empty placeholders satisfy Config::from_env
export POLLIS_DATA_DIR="$HOME/.local/share/pollis-tui" # own device identity
cargo run -p pollis-tui --bin pollis
```

Running the real binary needs a reachable remote Turso + DS, so it can't run in a
credential-less box. The **in-box smoke tests** sidestep that: they stand up the
DS in-process (exactly as the `flows` harness does) against a **local** libsql
file and force `DEV_OTP`, so the whole client path runs headless with no network:

```bash
cargo test -p pollis-tui        # unit tests + auth/sync smokes, all in-box
```

- `tests/common/mod.rs` — the shared rig: local libsql (`RemoteDb::connect_local`,
  gated behind pollis-core's `test-harness` dev-dep feature), an in-process
  `pollis-delivery` wired to just the routes the scenario hits, and a `TestClient`
  that signs up + drives the `pollis_tui` library through its own read-only
  `query_only_view` (proving the client never writes Turso directly — all writes
  go through the DS).
- `tests/sync_smoke.rs` — **the M2 gate.** Two clients share one DS + libsql; A
  opens a DM to B and sends while B is offline; B is driven *only* through
  `sync::sync_once` and must decrypt exactly A's message. Proves cross-client
  receive over real MLS.
- `tests/send_smoke.rs` — **the M3 gate: full MLS round-trip BOTH directions**,
  driven through the TUI's own `pollis_tui::send` write layer + `sync`. A sends
  "ping from A" and B decrypts it (the M2 direction); then B replies "pong from
  B" through the same send layer and A decrypts it (the NEW direction). Both
  ends finish holding both messages in send order, and both main handles are
  read-only — proving every write went through the DS. DM (not group) keeps the
  DS surface small (`dm/create` + `dm/accept` + `messages/send`, all pre-wired).
- `tests/restart_smoke.rs` — **restart→unlock→resync** (folds in #15; restores the
  DoD's quit→relaunch→unlock→resync coverage consolidated away in M2a). A (a
  **file-backed keystore** client, so its identity survives an `AppState` drop)
  receives B's message, then its `AppState` is dropped and rebuilt on the SAME
  `POLLIS_DATA_DIR` + libsql (`TestClient::{new_persistent,restart}` in the rig).
  `auth::boot` must report `Returning`, `auth::unlock` with the PIN succeeds, and
  after `sync_rounds` A can STILL read the pre-restart message.
- `tests/enroll_smoke.rs` — **the M4 enrollment gate: second device via sibling
  approval + working MLS leaf.** Device A (alice) is in a DM with Carol (a third
  user) and has sent a message. A fresh device B (same user, **its own
  `POLLIS_DATA_DIR`**) proves alice's email (`begin_enrollment`), requests
  enrollment (`request_enrollment`), A confirms the verification code and
  `approve`s, B polls to `Approved` and finishes (`set_pin` → `finalize` →
  `initialize_identity`). B then **sends** a message that both Carol and A
  receive + decrypt — proving B got a real leaf, not just an auth session.
- `tests/recover_smoke.rs` — **the M4 Secret-Key recovery gate.** Same shape, but
  B has no sibling to approve: it unwraps the account key from the server-stored
  `account_recovery` blob with the Secret Key surfaced on A's first signup
  (`verify_otp`'s `UserProfile.new_secret_key`), via `enroll::recover`. B then
  sends a message Carol + A decrypt.

Both M4 smokes need **per-device data dirs** — two devices of the same user
would otherwise collide on `pollis_{user_id}.db`, the file keystore, and
`accounts.json` (all keyed off `POLLIS_DATA_DIR`). The rig's
`TestClient::new_persistent_in(world, name)` pins each device to its own subdir
and repoints `POLLIS_DATA_DIR` just-in-time (`use_dir`, called from `activate`)
before every on-disk touch; safe because a test drives its clients sequentially
and each test file is its own process. The rig also wires the enrollment DS
routes (`/v1/auth/enrollment-request` session-gated, `/v1/enrollment/{approve,
reject}` device-signed, `/v1/security-events`).

## Sync model (M2, spec §6)

Media is off, so there is no LiveKit realtime inbox — the TUI **polls**.
`src/sync.rs` owns the canonical catch-up order, and `src/data.rs` owns the typed
read layer it (and the M2b left pane) share:

```text
sync_once(user):
  1. poll_mls_welcomes(user)              — drain Welcomes (may JOIN new groups/DMs)
  2. load_conversations(user)             — enumerate AFTER welcomes
  3. for each conversation: process_pending_commits  — advance MLS to head epoch
  4. for each conversation: get_channel_messages / get_dm_messages  — ingest + decrypt
```

Order is load-bearing: welcomes run **first** (a Welcome can create the group the
commits then replay into) and the message read runs **last** (it triggers the
interleaved replay+decrypt that surfaces a peer's message). One round can leave a
recovering member mid-handshake, so `sync_rounds` runs a fixed few (~4 settle an
interleaved catch-up). `spawn_loop` runs `sync_once` on a 3–5 s cadence in a
cancelable background task the Home UI owns (see below).

## The three-pane client UI (M2b, spec §8)

`Screen::Home` renders a three-pane client, all state living in
`HomeState` (`src/home.rs`) and rendered as a **pure function** of that state by
`ui.rs` (ratatui immediate mode — no retained widgets, no mutation during draw):

```
┌ header: " pollis — <user> · <open-conv> "            "<spinner> sync · N conversations" ┐
├───────────────┬──────────────────────────────────────────────────────────────────────┤
│ Conversations │  <open conversation name>                                              │
│  ▾ General    │  alice   hey there                                                     │
│    # welcome  │  bob     morning                                                       │
│    # random   │  …newest at the bottom…                                                │
│  Direct Msgs  │                                                                        │
│    @ bob      │                                                                        │
│  Requests     │                                                                        │
│    @ eve …    │                                                                        │
├───────────────┴──────────────────────────────────────────────────────────────────────┤
│ ↑/↓ move · Tab switch pane · Enter open · q quit                                       │
└───────────────────────────────────────────────────────────────────────────────────────┘
```

- **Left pane** — the conversation tree from `data::load_conversations`, flattened
  by `build_sidebar_rows` into groups (expandable to their `#` channels), a Direct
  Messages section, and a pending Requests section. Selection/focus use **solid**
  styling (a filled background, brighter when the pane is focused) — no glow, per
  the repo rule. Section headers are dim and skipped by navigation.
- **Main pane** — the open conversation's messages, oldest kept first and rendered
  **newest-at-bottom** via `visible_window` (bottom-anchored, top-padded when the
  buffer is short). Scrolling up past the loaded buffer prefetches the next older
  page through `data::*_messages`' `next_cursor` (`should_load_older` +
  `Action::LoadOlder`). Empty / first-load / pending-request / undecryptable states
  are all handled explicitly.
- **Header/status** — identity (username), the open conversation's name, and a
  live sync spinner + conversation count; the status line shows the keybindings or
  the latest transient status/error.

**Keys:** `↑/↓` or `j/k` move (sidebar selection, or message scroll when the main
pane is focused); `Tab` cycles focus; `Enter` toggles a group / opens a
conversation; `PageUp`/`PageDown` scroll by a screen; `q` or `Ctrl-C` quit. No
modal overlays — everything is a pane or a full-screen view.

### Sync → UI refresh wiring (the M2b integration)

The background `sync::spawn_loop` (4 s cadence) mutates the **local DB** off-thread;
the UI is decoupled from it and re-reads on its **own** faster tick so a synced
message surfaces within a frame:

1. On reaching `Screen::Home`, `Action::EnterHome` starts the `SyncLoop` (held on
   `App`) and queues the first `Action::Refresh`.
2. `main.rs`'s run loop is a `tokio::select!` over **{key input mpsc, a
   `UI_REFRESH` (750 ms) `tokio::time::interval`}**. A refresh tick (only while on
   Home) queues `Action::Refresh`; key input never blocks on a slow sync round.
3. `Action::Refresh` re-runs `data::load_conversations` (rebuilding the sidebar,
   preserving selection/expansion) and re-reads the open conversation's newest page,
   merging it in with `merge_messages` (dedup by id, incoming wins) so scrollback and
   scroll position survive. The sync spinner advances one frame per refresh.
4. On quit, `App::shutdown()` `cancel()`s the `SyncLoop` (letting the current round
   finish, with a 2 s timeout) **before** the `TerminalGuard` restores the terminal.

This satisfies §6's "a slow sync round must not freeze input": the loop and the UI
communicate only through the local DB + a UI-side timer, never a shared lock held
across a render. Rather than reach into `sync.rs` for a notify channel, the UI
polls the DB it already reads — a smaller, decoupled surface (the alternative §6
allows).

## The compose / create UI (M3b, spec §8/§11)

The interactive write layer on top of `src/send.rs`. Home has **three input
modes** (`HomeMode` in `home.rs`), and `on_home_key` dispatches to a per-mode
handler; the text buffer for compose/prompt is `App::input` (reused — the auth
screens don't run on Home). No modal overlays — every input is the bottom bar,
the desktop app's "replace the input bar" pattern. Solid accent border, no glow.

- **Navigate** (default) — tree movement + the command keys below. Reads that
  need a round-trip (open conversation, page history) return an `Action`; the
  write keys hand off to `begin_*` helpers that set the mode / queue an action.
- **Compose** — typing a message into the open conversation. Enter sends via
  `send::send_text` (empty/whitespace is a no-op), clears the buffer, pins to
  newest and `refresh_open()`s so the just-sent message appears immediately (the
  core stores it locally on send). Stays in compose on success (keep typing); a
  failure keeps the text for a retry. Esc leaves. An **un-accepted DM request
  can't compose** — the guard routes the user to accept it first.
- **Prompt** — an inline bottom bar collecting one value for a create/invite
  action (`PromptKind`). Enter submits (empty rejected), Esc cancels. `do_submit_prompt`
  dispatches on the kind: `new_group` / `new_channel` (scoped to the selected
  group via `context_group`) / `start_dm` (`search_user_by_username` →
  `start_dm`, a miss is a clean status-line error, never a panic) / `invite`.
  On success it `refresh_tree()`s so the new group/channel/DM surfaces; on
  failure the error lands on the status line and the prompt stays open to retry.

### Key bindings (Home)
| Key | Mode | Action |
|---|---|---|
| `↑/↓`, `j/k` | Navigate | Move selection (sidebar) / scroll (message pane) |
| `PageUp/Down` | Navigate | Scroll a screen |
| `Tab` | Navigate | Cycle sidebar ↔ message pane focus |
| `Enter` | Navigate | Sidebar: toggle group / open conversation. Message pane: start composing |
| `i` | Navigate | Compose a message in the open conversation |
| `a` | Navigate | Accept the highlighted pending DM request |
| `g` | Navigate | New group (prompt: name) |
| `c` | Navigate | New channel in the group in context (prompt: name) |
| `d` | Navigate | Start a DM (prompt: username/email → search → create) |
| `v` | Navigate | Invite to the group in context (prompt: username/email) |
| `q` / `Ctrl-C` | Navigate | Quit (`Ctrl-C` quits from any mode) |
| `Enter` | Compose/Prompt | Send / submit (empty is a no-op) |
| `Esc` | Compose/Prompt | Cancel back to Navigate |

The status line surfaces the mode's bindings when idle (`home_help`), so the
compose/accept/create/invite/quit keys are discoverable in-app.

**Pure, unit-tested M3b helpers** (`home.rs`): `is_blank` (empty-input guard),
`PromptKind::label`, `selected_dm_request` (accept target), `context_group`
(the group scoping new-channel/invite — walks up to the nearest heading, stops
at a section header so DM/request rows have no group). The interactive terminal
itself isn't in-box smoke-testable (no real TTY); correctness rides on these
pure helpers + the already-gated `send_smoke` round-trip under the write path.

## Milestones
- **M0** — skeleton: crate + workspace wiring, `AppState::new` boot, ratatui event
  loop, clean quit. ✅
- **M1** — auth: first-device signup (OTP→PIN→`initialize_identity`) + returning
  `get_session`→`unlock`. ✅
- **M2** — read: sync/read core — `data.rs` (conversation tree + paginated
  message reads) + `sync.rs` (§6 poll loop), gated by the cross-client `sync_smoke`.
  ✅
- **M2b** — the ratatui three-pane client UI (`home.rs` + `ui.rs`) wired to the
  M2 read/sync core, with the background sync → UI-refresh loop above. Pure
  helpers (tree flattening, selection, scroll windowing, pagination merge) are
  unit-tested; the interactive terminal itself isn't in-box smoke-testable. ✅
- **M3 (write CORE)** — the `src/send.rs` library layer (send, create
  group/channel, start/accept DM, invites) + gates: the bidirectional MLS
  round-trip (`send_smoke`) and the restart→unlock→resync cycle
  (`restart_smoke`). ✅
- **M3b (compose/create UI)** — the interactive layer on `send.rs`: compose+send,
  accept DM request, and the inline create/invite prompts (group/channel/DM/invite),
  with mode-aware key hints. Pure input-mode + selection-context helpers are
  unit-tested; the terminal interaction itself isn't in-box smoke-testable. See
  the compose/create UI section above. ✅
- **M4** — multi-device enrollment + Secret-Key recovery UX.

See the spec for the full command-surface → screen map and the polling sync model.
