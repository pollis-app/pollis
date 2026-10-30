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
  screens) + **#1337** (it reads everything the other clients write: the
  `_att`/`_txt` attachment envelope, newlines, wrapping, threads, quote replies,
  reactions, timestamps, mentions; and it can reply / edit / delete / react / open
  a thread; sign-up shows the Secret Key; `--help`/`--version`). It is a text
  client: an attachment renders as a line (`[image: photo.png, 2 KB]`), nothing is
  downloaded or uploaded, and media are out of scope. Remaining possible
  enhancement: realtime (replace the poll loop with a LiveKit inbox).

  **M4b screen flow** (spec §7): `verify_otp` returns `enrollment_required`. If
  false → first-device `SetPin` (unchanged). If true → **EnrollChoice** (↑/↓ or
  1/2): *Approval* → `request_enrollment` → **EnrollWaiting** (shows the
  verification code, polls `enrollment_status` on a tick; retry on rejected/
  expired) → on Approved, `SetPin` → `enroll::set_pin_and_finalize`; or *Recover*
  → **RecoverKey** (Secret-Key entry) → `enroll::recover` → same finalize tail.
  On an existing signed-in device, `E` opens **PendingEnrollments** (↑/↓ move,
  `a` approve, `r` reject). The list shows the requesting **device id and
  nothing else**: `a` opens **ApproveEnrollment**, where the approver TYPES the
  verification code shown on the NEW device (`Enter` approves, `Esc` back).

  That is #1096, and it is why code entry is its own screen rather than an inline
  field — `a`, `r`, `j`, `k` and `q` are all characters a verification code can
  contain, so the list's keymap and the code input cannot share a keystroke. The
  list used to display the Delivery Service's copy of the code and `a` approved
  with that same value, so the comparison the code exists for was performed by
  nobody: a DS that swapped the new device's ephemeral key and its stored code to
  match passed on one keystroke, and approving handed over the account private
  key. Input goes through `pollis_core`'s own
  `device_enrollment::normalize_enrollment_sas` — the single source of truth for
  the alphabet and length, shared with the desktop and mobile twins — so a
  lookalike is dropped rather than mapped (`O`→`0` would let a mis-read code
  compare equal).

  The pure branch/selection logic lives in `src/enroll_flow.rs`
  (`PinFlow`/`EnrollChoice`/`PollOutcome`/`ApprovalState`, unit-tested); the
  code-entry gate is `app.rs`'s pure `classify_code_entry` (`Enter` yields
  `Approve` only at the full length), also unit-tested; async work is in
  `app.rs`. `tests/enroll_smoke.rs` drives the whole two-device flow and asserts
  a near-miss code is REFUSED.

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
  AppState { Config, file Keystore, local SQLCipher DB }
        │ every remote read AND write (#987)
        ▼
   pollis-delivery ──▶ Turso libSQL
```

### Source layout
| File | Role |
|---|---|
| `src/main.rs` | Entry. Parses arguments (`cli.rs`) and checks for a terminal **before** anything else, then builds the multi-thread tokio runtime, `Config::from_env` → `AppState::new`, terminal setup, `tokio::select!` render/input loop (key input + UI-refresh tick), panic hook, clean sync-loop shutdown. |
| `src/cli.rs` | `--help` / `--version` (from `CARGO_PKG_VERSION`) and the not-a-terminal message (#1337). Pure, unit-tested; `tests/cli_args.rs` runs the real binary with an empty environment and no TTY. |
| `src/content.rs` | The message body as the other clients write it: `parse_content` (line-for-line port of mobile's `parseContent` / desktop's, `_att`/`_txt` envelope), `attachment_label`, `format_file_size`, `preview` (quote / prompt snippets), `mention_ranges` (same rule as core's `mention_tokens`), `replace_caption` (an edit keeps the attachments), `resolve_reaction` (`:shortcode:` via the vendored `scripts/emoji-shortcodes.json`, bare form like desktop), `fit_width` (sidebar ellipsis). |
| `src/timeline.rs` | The message pane's layout: displayed messages → `Block`s of wrapped, styled lines at a given width — date separators, `HH:MM sender` headers, kept newlines, word wrap (hard-break for over-long words), attachment lines, `↳ name: first words` quote context, reaction pills, `↪ N replies`, mention + selection highlight, lost-message runs. `layout_open` is shared by the renderer and the key handler (`scroll_to_show` keeps the selected message on screen). Pure, unit-tested per message shape. |
| `src/terminal.rs` | `TerminalGuard` — RAII enter/leave raw mode + alt screen; restores the terminal on **every** exit path (quit, `?`, panic). |
| `src/app.rs` | `App` state machine: `Screen` enum, synchronous `on_key` (auth + Home navigation/compose/prompt/confirm), async `run(Action) -> Option<Action>` (returns a follow-up action), background `SyncLoop` ownership + `shutdown()`. M3b adds the `SendMessage`/`AcceptDm`/`SubmitPrompt` actions + the `begin_*`/`do_*` compose/create handlers; #1337 adds `Screen::SaveSecretKey`, the message actions (`begin_reply`/`begin_edit`/`begin_delete`/`begin_react`/`begin_open_thread`, `Action::DeleteMessage`/`OpenThread`), `refresh_extras` (reactions + thread counts + open thread) and `land_on` (focus the conversation just created). #1335 adds `R` (`begin_retry` → `Action::RetryMessage` → `do_retry`) for a message the durable outbox could not deliver, and routes `x` on an unsent row to a discard instead of a delete. |
| `src/home.rs` | The three-pane model + **pure, unit-tested** helpers: sidebar flattening (`build_sidebar_rows`), selection movement (`step_selection`/`clamp_selection`), bottom-anchored scroll windowing (`visible_window`), scrollback prefetch (`should_load_older`), page merge/dedup (`merge_messages`). M3b adds the input-mode model (`HomeMode` = Navigate/Compose/Prompt, `PromptKind` + its `label()`), the empty-input guard (`is_blank`), and the pure selection-context helpers (`selected_dm_request`, `context_group`). #1337 adds `HomeMode::ConfirmDelete`, `PromptKind::React`, `ComposeTarget` (new / reply / edit), `ThreadView`, and on `OpenConversation` the reactions, thread counts, selected message id and `displayed()` (thread replies filtered out of the channel view; root + replies in a thread view), `step_message`, `select_conversation`. |
| `src/auth.rs` | Thin wrappers over `pollis_core::commands::{auth,pin}` that encode the M1 call order. No forked logic. |
| `src/data.rs` | Typed read layer: `load_conversations` (tree) + `channel_messages`/`dm_messages` (paginated), plus (#1337) `reactions` (one batched DS read via core `get_reactions_for_messages`), `thread_counts` (`list_thread_summaries`) and `thread_replies` (`read_thread_messages`), both local. Shared by `sync.rs` and the Home UI. |
| `src/send.rs` | **M3 write CORE.** Typed passthroughs over the exact core writes (`send_message`, `create_group`, `create_channel`, `create_dm_channel`, `accept_dm_request`, `invite_to_group`) + ergonomic shorthands with UI defaults baked in (`send_text`, `new_group`, `new_channel`, `start_dm`, `accept_dm`, `invite`), and (#1337) `send_text_with` (reply_to_id / thread_id), `edit`, `delete`, `toggle_reaction`, and (#1335) `retry` / `discard` over the outbox's `retry_message` / `discard_unsent_message` — the same core calls desktop and mobile make. No forked logic — every write routes through the DS via the core fn. M3b calls one fn per action. |
| `src/enroll.rs` | **M4 DATA core.** Typed, order-enforcing wrappers over `pollis_core::commands::device_enrollment` for the two "add a terminal to an existing account" flows: sibling-approval enrollment (`request_enrollment`/`enrollment_status`/`finalize` on the new device, `pending_requests`/`approve`/`reject` on the existing one) and Secret-Key recovery (`recover`, plus the last-resort `reset_and_recover`). No forked logic. Re-exports `EnrollmentHandle`/`EnrollmentStatus`/`PendingEnrollmentRequest` for the M4b UI. |
| `src/sync.rs` | §6 poll loop: `sync_once`/`sync_rounds` + `spawn_loop` (cancelable background `SyncLoop`). |
| `src/ui.rs` | Pure `render(frame, &app)` — header (identity · open-conversation name · sync spinner) / three-pane Home body / auth card / Secret Key card / status line, and on Home an always-visible key-hint bar (`home_help`, per focused pane and mode). `message_viewport` gives the input loop the pane's inner size from the same `home_layout` the renderer uses. |

### Key design points (from the spec)
- **Multi-thread runtime is mandatory.** `pollis-core`'s DB/keystore paths use
  `spawn_blocking`; a current-thread runtime deadlocks. Hence
  `#[tokio::main(flavor = "multi_thread")]`.
- **No `media` feature; `os-keystore` ON.** The crate depends on `pollis-core`
  with `default-features = false`, so `media` (livekit/libwebrtc/cpal) is **off**
  — voice/screenshare/camera are out of scope, and `data::load_conversations`
  drops voice channels from the tree (as mobile does) so they never show up as
  empty text channels. `os-keystore` is deliberately
  **on** (#879): the shipped `pollis` binary must prefer the OS keychain. Where
  no keychain answers — the server-over-SSH case that is this client's likely
  home — it falls back **at runtime** to the machine-bound encrypted file store
  (#882), so the same binary works on both without ever writing plaintext.
  `pollis-tui/src/auth.rs` carries a guard test that fails if the feature is
  dropped again.
- **Everything goes through the Delivery Service.** Writes since #419, reads
  since #987 — `POLLIS_DELIVERY_URL` is **mandatory** config and is now the only
  backend the TUI can reach at all: it holds no database credential, and neither
  does `pollis-core`. The TUI invents no new backend path.
- **Own device identity.** Set `POLLIS_DATA_DIR` (e.g.
  `~/.local/share/pollis-tui`) so the TUI's file keystore + local SQLCipher DB do
  not share identity with the desktop app — it enrolls as its own device with its
  own `device_id` and MLS leaf. Unset, it uses `pollis_core::db::local::dirs_path()`,
  the platform default (on macOS the desktop app's own
  `~/Library/Application Support/com.pollis.app`); `pollis --help` prints the
  directory in effect.
- **The stderr log is owner-only, and its directory matters.** `main.rs` `dup2`s
  fd 2 into `pollis-tui.log` so `pollis-core`'s `eprintln!` output does not
  scroll over the TUI. The file lands in the data dir — `POLLIS_DATA_DIR`, else
  `dirs_path()`, the same directory as the database and keystore. It used to fall
  back to the shared, world-*writable* OS temp dir when `POLLIS_DATA_DIR` was unset
  (#1337). It is created 0600
  (`pollis_core::private_fs::FILE_MODE`), and one left by an older build is
  tightened through the open handle. The two `auth` log lines that carried the
  account's email now mask it (`pollis_core::util::mask_email`, the same shape
  the DS uses in `pollis-delivery/src/redact.rs`), so a signup no longer writes
  the user's real address into it in the clear (#1000).
- **Arguments and the terminal come first (#1337).** `main` parses `--help` /
  `--version` before `Config::from_env`, so both work with no TTY and no
  environment (a dev build without its env used to die on `missing env var:
  R2_S3_ENDPOINT`); any other argument is refused with exit 2. Without a TTY on
  stdin and stdout it says so on stderr and exits 1, before stderr is redirected —
  it used to exit 1 with `entering raw mode: Device not configured` only in the
  log. `cli-release.yml` stamps the release version in (`scripts/bump-version.sh`)
  so `--version` is the shipped version, not the placeholder.
- **The minimum-version check (#1611)**, as platform `cli`. `--help` /
  `--version` never touch it. Without a terminal, no fetch is paid for before
  the usage error, but a cached block still prints and exits **3**
  (`cli::EXIT_UPGRADE_REQUIRED`). On a terminal it runs before anything opens:
  a cache that allows this build starts the UI at once and checks in the
  background; otherwise it waits for `client_policy::check`. A blocked build
  prints what happened, the operator message if any and the `cli-install.sh`
  command, and exits 3. Unreachable or malformed policies fail open; a cached
  block holds offline. Every DS request carries `X-Pollis-Client-*`; a block
  found in the background or by a DS 426 leaves the UI and exits the same way. Tests:
  `tests/client_policy.rs` (including a pty run); design:
  [client-policy.md](./client-policy.md).
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
                       └► Fresh ─► Email ─► Otp ─► SaveSecretKey ─► SetPin ──set_pin+init──► Home
```

**SaveSecretKey (#1337).** A first-device `verify_otp` returns the account's
Secret Key (`UserProfile.new_secret_key`), and only then. The CLI never showed it,
so a CLI-only account had no recovery path. Now `App` takes it out of the profile
the moment it arrives (into a `Zeroizing` buffer — wiped when dropped, never
logged or persisted) and shows it, as desktop's `SaveSecretKeyScreen` and mobile's
`emergency-kit` screen do. `s` writes the Emergency Kit file — the same document
desktop downloads, pinned to desktop's `auth.json` by a unit test — owner-only to
the **current directory** (never the data dir: a recovery key beside the keystore
it recovers is no backup). Enter moves to a confirm phase where the user must type
or paste the key back (desktop's rule: whitespace, dashes, `A3-` and case
ignored); only a match goes on to `SetPin`. Esc shows the key again.

## Build & run (headless, in-box)

**Hands-on against the dev DS (#1337).** `cargo run -p pollis-tui --example
message_shapes_peer -- <email>` (with `POLLIS_DATA_DIR` set to a fresh dir and the
dev env sourced) signs up a second account, prints its handle, waits for a DM
request from the client under test (`d` + the handle), accepts it and sends one of
every message shape the other clients produce — multi-line, long, a captioned photo
(`_att` envelope), a mention of the requester, a thread reply and a quote reply to
the first message, and a reaction. Use a DEBUG build of `pollis` for this (debug
builds use the encrypted-file keystore and never touch the OS keychain).

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

- `tests/common/mod.rs` — the shared rig: two local libsql files
  (`pollis_delivery::db::Db::connect_local`), the REAL `pollis-delivery` router
  mounted in-process, and a `TestClient` that signs up + drives the `pollis_tui`
  library. Since #987 it mounts the real router rather than a hand-rolled subset:
  every client read is now a DS endpoint too, so a test double would have had to
  reimplement twenty more handlers whose only job was to agree with the
  originals. The client has no database handle at all — "everything went through
  the DS" is a compile-time fact now, pinned by
  `pollis-core/tests/no_client_side_remote_reads.rs`, not a `query_only` PRAGMA
  catching a stray write at runtime.
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
  data dir + libsql (`TestClient::{new_persistent,restart}` in the rig).
  `auth::boot` must report `Returning`, `auth::unlock` with the PIN succeeds, and
  after `sync_rounds` A can STILL read the pre-restart message.
- `tests/enroll_smoke.rs` — **the M4 enrollment gate: second device via sibling
  approval + working MLS leaf.** Device A (alice) is in a DM with Carol (a third
  user) and has sent a message. A fresh device B (same user, **its own
  data dir**) proves alice's email (`begin_enrollment`), requests
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
`accounts.json` (all keyed off the data dir). The rig's
`TestClient::new_persistent_in(world, name)` pins each device to its own subdir
and repoints the process at it just-in-time (`use_dir`, called from `activate`)
before every on-disk touch.

That repointing goes through **`pollis_core::db::local::set_data_dir`**, never
`std::env::set_var("POLLIS_DATA_DIR", ..)` (#923). The rig always has other
threads live — the in-process DS, each `App`'s background sync loop — and
`setenv` racing another thread's `getenv` is undefined behaviour, which shows up
as a rare load-dependent failure and nothing else. `set_data_dir` is a locked
write; `POLLIS_DATA_DIR` still works and is still what a second dev instance and
the mobile bridge use. The rig also wires the enrollment DS
routes (`/v1/auth/enrollment-request` session-gated, `/v1/enrollment/{approve,
reject}` device-signed, `/v1/security-events`).

Because the data dir is process-global, **every test file here holds exactly
one `#[tokio::test]`**. Cargo runs a binary's tests on parallel threads, and a
second test in the same binary repoints the dir mid-signup of the first
("Couldn't set PIN: no active user; call verify_otp first" — exactly how
#1041's PR first went red). A new scenario gets a new file, e.g.
`tests/ui_e2e.rs` (the rendered-screen two-client gate) and
`tests/ui_messages_e2e.rs` (#1337: every message shape another client
produces — attachment envelope, multi-line, long, thread reply, quote reply,
reaction, mention — asserted on the rendered screen, plus reply / edit / delete /
react / thread driven by keystrokes, focus landing on a UI-started DM, and the
hint bar on first entry), `tests/cli_args.rs` (the real binary's `--help` /
`--version` / no-TTY behaviour with an empty environment), and
`tests/ui_race_e2e.rs` (the same scenario with the rendezvous points forcing
the recipient to lose the epoch-0 external-join race).

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

**Keys:** `↑/↓` or `j/k` move (sidebar selection, or message selection when the
main pane is focused); `Tab` cycles focus; `Enter` toggles a group / opens a
conversation; `PageUp`/`PageDown` scroll by a screen; `q` or `Ctrl-C` quit. No
modal overlays — everything is a pane or a full-screen view.

### Rendering what the other clients write (#1337)

The main pane lays out through `timeline::layout_open` (see the source table):

```
── Today ────────────────────────────────────
14:05 bob  ROOT first line
      second line                      ← newlines kept
       🎉 1   👍 1                       ← reactions (own ones highlighted)
      ↪ 2 replies · t to open           ← thread root; replies are NOT in the timeline
14:06 bob  LONGSTART word word word …
      word word … LONGEND              ← wrapped, never cut off
14:06 bob  CAPTION_HERE
      [image: photo.png, 2 KB]         ← `_att` envelope → caption + attachment line
      ↳ bob: ROOT first line …          ← quote-reply context (`reply_to_id`)
14:07 bob  hey @alice look             ← a mention of you (or @all) is highlighted
```

- **Threads.** The channel view is `displayed()`: messages with a `thread_id` are
  filtered out, as other clients file them under the thread; the root carries
  `↪ N replies` from `list_thread_summaries`. `t` opens a thread view (root +
  `read_thread_messages`), where `i` replies into the thread (`thread_id` = root)
  and `Esc` closes it.
- **Reactions** are read once per completed sync round (and after the user's own
  reaction) for the newest 60 displayed messages, in ONE request —
  `get_reactions_for_messages` in `pollis-core`, the batched form of
  `get_reactions` (the wire read was always batched; the CLI has no realtime
  invalidation, so per-message reads would be one request per visible message per
  round).
- **Selection.** With the message pane focused, ↑/↓ select a message (by id, so
  it survives page merges); the view scrolls to keep it on screen
  (`scroll_to_show`), and selecting near the top of what is loaded prefetches
  history. Down past the newest releases the selection and follows new messages
  again.

### Focus and hints (#1337)

The focused pane has a thick accent border and an accent title bar; the other a
thin dim one. Creating a group / channel / DM highlights its sidebar row and opens
it with the message pane focused (`land_on`) — the highlight used to stay where it
was, so after starting a DM ↑/↓ scrolled an empty pane and the DM was unreachable
without knowing to press Tab. The key-hint bar has its own row under the status
line, so it shows from the first frame of Home (it used to share the status line
and stay hidden behind "You're signed in." until Esc). Sidebar names that do not
fit end in `…` (`fit_width`) instead of being clipped.

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
| `↑/↓` (message pane) | Navigate | Select a message (newest first; past the newest releases the selection) |
| `r` | Navigate | Quote-reply to the selected message (`reply_to_id`) |
| `e` | Navigate | Edit the selected message — own only; an attachment message keeps its attachments (`replace_caption`) |
| `x` / `Delete` | Navigate | Delete the selected message — own only; asks `y`/`n` in the input bar |
| `+` | Navigate | React (prompt: emoji or `:shortcode:`); reacting with one you already have removes it, like desktop's pill |
| `t` | Navigate | Open the selected message's thread |
| `Esc` | Navigate | Close the thread → drop the selection → back to the sidebar |
| `Alt-Enter` / `Ctrl-J` | Compose | New line |
| `y` / `n` | Confirm delete | Delete / keep |

The hint bar (its own row under the status line, always visible on Home) shows
the bindings for the focused pane and mode (`home_help`), so every key is
discoverable in-app.

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

## The durable outbox in the CLI (#1335)

The CLI sends through the same core `send_message`, so it gets the durable
outbox for free: the row is stored before any network work, and the drain the
core spawns after `unlock` delivers anything a killed CLI left behind. A message
whose delivery failed (`send_state` `failed`, or `rejected` when no retry can
fix it) carries a red `✗ not sent — R retry · x discard` line under it
(`timeline::UNSENT_MARK`, drawn by `finish`); a `pending` one is drawn like a
sent one, as in the apps. `R` retries it under the same id; `x` on it discards
it (the core withdraws it from everyone first if an attempt may have landed).
`tests/outbox_smoke.rs` drives a failed send, its retry (delivered once to the
peer) and a discard (never reaches the peer) through `pollis_tui::send`;
`timeline::tests::an_unsent_failure_is_marked_with_its_keys` pins the marker.
