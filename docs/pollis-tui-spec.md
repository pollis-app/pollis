# pollis-tui — Terminal Client Design Spec

**Status:** design complete, ready for implementation handoff.
**Author:** Fable (design) → inner agent / Opus (execution).
**Scope:** a persistent, full-screen terminal messaging client for Pollis, built
directly on `pollis-core` with **no Tauri, no IPC, no WebView**. Text messaging
only in v1 (voice/screenshare/camera/attachments explicitly out — see §9).

The premise: `pollis-core` is a shell-agnostic Rust crate that already exposes the
entire client surface (auth, MLS, groups, channels, DMs, messages) as plain async
functions taking `&Arc<AppState>`. The React app reaches them over Tauri `invoke`;
the TUI calls them **in-process, directly**. This is *less* indirection than the
desktop app, and it builds/tests headlessly in-box (`--no-default-features`).

---

## 1. Goals & non-goals

**Goals**
- A daily-driver terminal client: sign in, browse groups/channels/DMs, read and
  send text messages over real MLS end-to-end encryption, stay synced.
- Persistent: file-backed keystore + local SQLCipher DB; stays enrolled across
  launches like a normal device.
- Zero new backend: reuse the exact command surface, Delivery Service, and Turso
  the desktop app uses. The TUI is just another **device** of the user.
- Ship as a single static binary; build + smoke-test in-box.

**Non-goals (v1)**
- Voice, screenshare, webcam (require the `media` feature + a GUI surface).
- Attachments / file upload-download (R2) — fast-follow, not blocked by `media`.
- Multi-device *enrollment* onboarding UX — see §7 for the decision point; v1
  targets first-device signup + Secret-Key recovery, enrollment as a milestone.
- Realtime push (LiveKit inbox). v1 **polls** on a timer (§6). Realtime is a
  later enhancement gated on non-stub LiveKit.

---

## 2. Architecture

```
┌─────────────────────────────────────────────┐
│  pollis-tui  (new binary crate)             │
│                                             │
│  ratatui + crossterm  ── render/input       │
│         │                                   │
│  App (UI state machine)                     │
│         │  direct async calls               │
│         ▼                                   │
│  pollis_core::commands::*  (&Arc<AppState>) │
│         │                                   │
│  AppState { Config, RemoteDb, log_db,       │
│             file Keystore, local DB }       │
└─────────────────────────────────────────────┘
        │ reads (direct)        │ writes (via DS)
        ▼                       ▼
      Turso libSQL        pollis-delivery  ──► LiveKit / R2 (unused v1)
```

- **No Tauri.** Do **not** build a `tauri::App` or use the `#[tauri::command]`
  shims in `src-tauri/`. Those are 1:1 forwarders. Call
  `pollis_core::commands::<module>::<fn>(args…, &state)` directly.
- **Reads go direct to Turso; writes go through the Delivery Service.** This is
  the post-#419 model — `send_message`, group/DM/invite/reaction ops all
  `ds_post_ok` to `pollis_delivery_url`, which is therefore **mandatory** config.
- **Runtime:** multi-thread Tokio (`rt-multi-thread`). The DB/keystore paths use
  `spawn_blocking`, so a current-thread runtime deadlocks.
- **Incoming messages surface by polling**, not a sink push (the canonical state
  is the local DB). Use the core-provided `NoopSink` where a sink is required.

### Crate layout
Add `pollis-tui` to the root `Cargo.toml` `[workspace].members`. `pollis-tui/Cargo.toml`:

```toml
[package]
name = "pollis-tui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[[bin]]
name = "pollis"          # ships as the `pollis` terminal command
path = "src/main.rs"

[dependencies]
pollis-core = { path = "../pollis-core", default-features = false }  # media + keyring OFF
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
ratatui = "0.28"
crossterm = "0.28"
anyhow = "1"
```

Import surface: `use pollis_core::{state::AppState, config::Config, keystore::{Keystore, OsKeystore, default_os_keystore}, commands::*};`. The lib is `pollis_core`.

---

## 3. Client construction

`AppState` has two constructors (`pollis-core/src/state.rs:146-204`):

```rust
pub async fn new(config: Config) -> Result<Self>                    // wires default_os_keystore + connects both DBs
pub fn new_with_parts(config, remote_db, log_db, keystore) -> Self  // explicit parts
```

For the persistent TUI use the high-level path:

```rust
let config = Config::from_env()?;                       // or literal (see §4)
let state = Arc::new(AppState::new(config).await?);     // uses default_os_keystore() = file-backed when keyring off
```

`AppState::new` connects `remote_db` (Turso) and `log_db` (falls back to
`remote_db` when `LOG_DB_URL` unset). Every command takes `state: &Arc<AppState>`.

**Note:** `AppState` holds **no** `EventSink` field — sinks are passed only to
media/realtime commands the TUI never calls. No sink wiring needed.

---

## 4. Configuration & credentials

`Config` (`pollis-core/src/config.rs:3-25`) fields:
`turso_url, turso_token, log_db_url?, log_db_token?, r2_* , livekit_*, pollis_delivery_url?`.

`Config::from_env` (`config.rs:27-70`) requires `TURSO_URL, TURSO_TOKEN,
R2_S3_ENDPOINT, R2_ACCESS_KEY_ID, R2_SECRET_KEY, R2_PUBLIC_URL`; the rest default.

**For a text-only TUI:** `turso_url`/`turso_token` and `pollis_delivery_url` are
the real requirements. R2/LiveKit fields are unused in v1 and may be empty
strings (as `Config::for_test` does, `config.rs:88-117`).

**Credential distribution — reuse the desktop mechanism (no new secret handling).**
The desktop app bakes first-party creds at build time via `option_env!` (Doppler →
CI env), with a runtime `std::env::var` fallback (`config.rs:65-67`). `pollis-tui`
inherits this for free by using `Config::from_env`:
- **Release build** (in the desktop-release-style pipeline with Doppler env set):
  Turso/DS creds bake in — the shipped binary "just works", same trust model as
  the desktop app (the Turso token is a *shared app credential*; per-user identity
  is the OTP/session/MLS layer, not this token).
- **Dev build:** creds come from the runtime environment (`.env`/exported vars).

⇒ **No bespoke credential UX.** The user never pastes a Turso token. Onboarding is
purely the OTP + PIN flow (§7).

### Data directory — the TUI is its own device
The file-backed keystore + local DB live under `POLLIS_DATA_DIR` (default
`~/.local/share/pollis`, `keystore.rs:40-80`). **Set a distinct
`POLLIS_DATA_DIR` for the TUI** (e.g. `~/.local/share/pollis-tui`) so it does not
share device identity with the desktop app. Consequence: the TUI enrolls as a
**separate device** with its own `device_id` and its own MLS leaf — consistent
with the multi-device model. See §7 for the enrollment implication.

---

## 5. Keystore

`OsKeystore` (`keystore.rs:277-292`) resolves to a **file-backed JSON store**
whenever the `os-keystore` feature is off (which it is under
`--no-default-features`) — persistent, zero dbus dependency. Instantiate via
`default_os_keystore()`; `AppState::new` already does this. No custom keystore
code required. (`InMemoryKeystore` exists for a future ephemeral/burner mode — out
of scope here.)

Keystore trait (`keystore.rs:255-272`): `store/load/delete` + `*_for_user`
variants. The TUI never calls these directly — the auth/pin/mls commands do.

---

## 6. The sync model (polling)

Media-off means no LiveKit realtime inbox, so v1 **polls**. A background Tokio
task runs the catch-up loop on a timer and signals the UI to refresh.

**The catch-up sequence per online cycle** (from `flows/model.rs` `converge` +
`harness.rs`):

```
for round in 0..N (N≈4 to settle recovering-member ↔ committer ping-pong):
    poll_mls_welcomes(&state, user_id)                 // per user — drains queued Welcomes
    for each conversation the user is in:
        process_pending_commits(&state, conv_id, user_id)   // per conversation — replay to head epoch
    get_channel_messages(user_id, active_channel_id, limit, cursor, &state)  // drives interleaved decrypt
```

- `poll_mls_welcomes` **before** `process_pending_commits` (a Welcome may create
  the group the commits then replay into). The fetch is **last** — it is what
  triggers the interleaved replay+decrypt (`catch_up_mls_group_interleaved`,
  `ingest.rs:105`), which enumerates every sibling channel of the group.
- **Conversation enumeration:** `list_user_groups(user_id)` → for each,
  `list_group_channels(group_id)`; plus `list_dm_channels(user_id)` and
  `list_dm_requests(user_id)`. Run `process_pending_commits` for each channel/DM.
- **Cadence:** poll every ~3–5 s while foregrounded. This is the latency/CPU
  trade-off; document it. (A later realtime path can replace the timer.)
- **Threading:** the poll loop runs on a spawned task; it must not block the
  render loop. Communicate via a `tokio::sync::mpsc`/`watch` channel → the UI
  re-reads local state and redraws. Never hold a DB lock across a render.

Pagination: `get_channel_messages` returns newest-first up to `limit` (default 50)
with `next_cursor: Option<MessageCursor>` (`messages/types.rs:56-72`). To scroll
into history, pass the cursor back for older pages — **page to the end** if you
need full history (this is exactly the #442 lesson: a single page is not the whole
conversation).

---

## 7. Onboarding & auth flow

Call order (verified against `auth.rs` + `TestClient::sign_up`, `harness.rs:2387`).

### First-device signup
```
request_otp(&state, email)                     // auth.rs:74  — sends OTP (Resend), or DEV_OTP in dev
verify_otp(&state, email, code) -> UserProfile // auth.rs:104 — authenticates; LEAVES LOCAL DB CLOSED
set_pin(newPin, oldPin=None, &state)           // commands::pin — opens the per-user SQLCipher DB
initialize_identity(&state, user_id)           // auth.rs:40  — publishes MLS key package + initial poll
```
**Critical gotcha:** `verify_otp` deliberately leaves the local DB *closed*. Until
`set_pin` (or `unlock`) opens it, every DB-touching command fails with
`"Not signed in"` (`read.rs:119`). The PIN is the DB-unlock factor (4 digits, the
existing model — `pin.rs`).

### Returning launch
```
get_session(&state) -> Option<UserProfile>     // auth.rs:545 — rehydrate profile from keystore
unlock(pin, &state)                            // commands::pin — re-open the local DB
// then enter the normal sync loop (§6)
```

### Additional-device enrollment — DECISION POINT
Because the TUI is its own device (§4), a user who already has the desktop app is
enrolling a **second device**. That requires either (a) approval from an existing
device (`device_enrollment.rs`, the `enroll_second_device` flow the tests
exercise) or (b) Secret-Key recovery. The enrollment approval UX is heavier (an
existing device must approve).

**v1 recommendation:** build the auth/session/pin plumbing to support both, ship
**first-device signup + Secret-Key recovery** first (covers "I'm setting up
Pollis fresh from a terminal" and "I have my Secret Key"), and treat
**interactive second-device enrollment approval** as milestone M4 (§11). Confirm
with product before locking this — see Open Decisions.

---

## 8. Command surface → TUI screens

All functions in `pollis-core/src/commands/`; each takes `&Arc<AppState>`.

| Screen / action | Core call(s) |
|---|---|
| Login | `request_otp` → `verify_otp` → `set_pin`/`initialize_identity`; returning: `get_session` → `unlock` |
| Profile | `get_user_profile(user_id)`, `update_user_profile(...)` |
| Group list (left pane) | `list_user_groups(user_id)` → per group `list_group_channels(group_id)` |
| DM list | `list_dm_channels(user_id)`, `list_dm_requests(user_id)` |
| Open channel | `get_channel_messages(user_id, channel_id, Some(50), None, &state)` → `MessagePage` |
| Open DM | `get_dm_messages(user_id, dm_channel_id, Some(50), None, &state)` |
| Scroll to history | re-call with `cursor = page.next_cursor` |
| Send message | `send_message(conversation_id, sender_id, content, reply_to_id, sender_username, &state)` |
| Start DM | `create_dm_channel(creator_id, member_ids, &state)` |
| Accept DM request | `accept_dm_request(dm_channel_id, user_id, &state)` |
| Create group | `create_group(name, description, owner_id, Some(true), Some(false), &state)` |
| Create channel | `create_channel(group_id, name, description, channel_type, creator_id, &state)` |
| Invite to group | `send_group_invite(group_id, inviter_id, invitee_identifier, &state)` |
| Pending invites | `get_pending_invites(user_id)` → `accept_group_invite` / `decline_group_invite` |
| Members | `get_group_members(group_id)` |
| Background sync | `poll_mls_welcomes`, `process_pending_commits` (§6) |
| Search user | `search_user_by_username(username, &state)` |

Also available if useful: reactions (`add_reaction`/`remove_reaction`/`get_reactions`),
`edit_message`, `delete_message`, `search_messages`, `list_channel_previews`,
join-request flow (`request_group_access`/`approve_join_request`/…).

### UX layout (ratatui)
- **Three-pane**: left = groups→channels + DMs tree; center = message list of the
  active conversation (newest at bottom, scrollback pages via cursor); bottom =
  input line. A status/header bar shows sync state ("synced • 3s ago") and identity.
- **Keybindings** (Slack/irssi-familiar): `Ctrl-K` fuzzy jump to conversation;
  `j/k` or arrows to move; `Enter` to open/send; `Tab` to cycle panes; `gg/G`
  scroll; `Ctrl-C` quit. Composer supports multi-line (`Alt-Enter` newline).
- **No modals-equivalent needed** (this is a TUI, not the GUI's rule) but keep
  flows inline: confirmations are a one-line prompt in the input bar.
- **Honesty in the status bar**: show "polling" cadence and last-sync; when a send
  is in flight vs delivered vs failed, reflect it (never silently drop — mirrors
  the app's "messages must work" doctrine).

---

## 9. Feature-flag scoping (what's IN vs OUT)

Builds with `pollis-core` `--no-default-features` (drops `media` = livekit/
libwebrtc/cpal/rodio/APM, and `os-keystore` = keyring/dbus → file keystore).

**OUT of v1** (behind `media`, `commands/mod.rs:14-60`): voice (`voice/*`),
screenshare (`screenshare/*`), camera (`camera/*`), sfx, realtime LiveKit rooms
(stubbed). **Do not reference these.**

**IN** (compile media-off): auth, pin, user, groups, channels, messages, dm, mls,
blocks, push, safety, transparency, update, `livekit_jwt` (pure). Attachments
(`r2.rs`) compile but are **deferred to v1.1** (need R2 creds + a terminal
file-picker UX) — scope text-only first.

---

## 10. Build, gate & test (in-box)

The box is a headless Rust rig for exactly this surface.

- **Build:** `cargo build -p pollis-tui` (media/keyring off via the crate's
  `default-features = false`). Must compile in-box (no webkit/ALSA/dbus).
- **Lint:** `cargo clippy -p pollis-tui -- -D warnings`.
- **Headless core regression stays green:**
  `cargo test -p pollis --no-default-features --features test-harness --test flows`
  (proves the new crate didn't perturb the workspace).
- **Smoke test (optional, in-box):** point `TURSO_URL`/`TURSO_TOKEN` at the
  disposable test DB + `POLLIS_DELIVERY_URL` at a local `pollis-delivery`, run the
  binary headlessly through a scripted signup→send→read against the same infra the
  flows harness uses. (The flows harness already stands this up; reuse `.env.test`.)
- **Definition of Done (v1):** builds + clippy-clean in-box; headless flows
  regression green; a documented manual run that signs up, creates a group/channel,
  sends and receives a message decrypted end-to-end, and survives a
  quit→relaunch→unlock→resync cycle; `.codesight/wiki` page added; no `media`
  references; single-line commits, no co-author trailer.

---

## 11. Phased milestones

- **M0 — Skeleton:** crate + workspace wiring; `AppState::new` boot; ratatui event
  loop; quit. Gate: builds in-box.
- **M1 — Auth:** first-device signup (OTP→PIN→initialize_identity) + returning
  `get_session`→`unlock`. Gate: signs in against test DB/DS.
- **M2 — Read:** group/channel/DM tree; open a conversation; paginated message
  list; background poll loop (§6) refreshing the view. Gate: sees a message sent by
  another client.
- **M3 — Write:** send text; create group/channel; start/accept DM; invites. Gate:
  full MLS round-trip both directions in-box.
- **M4 — Multi-device enrollment** (decision-gated, §7): interactive second-device
  enrollment approval + Secret-Key recovery UX.
- **v1.1 — Attachments** (R2), reactions/edits/threads polish.
- **Later — Realtime** (replace polling when non-stub LiveKit is available), and a
  scriptable non-interactive CLI mode.

Each milestone is an independently gate-able chunk suitable for one inner-agent
pass.

---

## 12. Open decisions (for product)

1. **Onboarding scope for v1** — first-device signup + Secret-Key recovery now,
   interactive second-device enrollment at M4? (Recommended.) Or is
   second-device the *primary* use case (people adding a terminal to an existing
   account), making M4 a v1 blocker?
2. **Binary name / distribution** — ship as `pollis` via
   `curl https://pollis.com/cli | sh` (static musl), reusing the release pipeline's
   Doppler cred baking? (Recommended.)
3. **Data-dir isolation** — confirm the TUI is a *separate device*
   (`POLLIS_DATA_DIR=~/.local/share/pollis-tui`) rather than sharing the desktop's
   identity. (Recommended: separate — sharing device keys across processes is
   fragile and off-model.)

---

## Appendix — key file references
- `pollis-core/src/state.rs:146-204` — `AppState::new` / `new_with_parts`
- `pollis-core/src/config.rs:3-70` — `Config` + `from_env`
- `pollis-core/src/keystore.rs:255-336` — `Keystore` trait, `OsKeystore`, `default_os_keystore`
- `pollis-core/src/sink.rs:12-36` — `EventSink`/`NoopSink` (no sink needed)
- `pollis-core/src/commands/auth.rs:40,74,104,545` — identity/otp/session
- `pollis-core/src/commands/pin.rs` — `set_pin`/`unlock`/`lock`
- `pollis-core/src/commands/messages/{send.rs:9,read.rs:83,409,types.rs:56}` — send/read/pagination
- `pollis-core/src/commands/mls/{welcomes.rs:182,group_state.rs:1196}` — sync calls
- `pollis-core/src/commands/dm.rs` / `groups/*` — DM + group surface
- `src-tauri/tests/flows/{harness.rs,model.rs}` — reference construction + sync loop
- Root `Cargo.toml` `[workspace].members` — add `pollis-tui`
