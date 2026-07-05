# Terminal Client (`pollis-tui`)

A persistent, full-screen **terminal** messaging client for Pollis, built
directly on `pollis-core` with **no Tauri, no IPC, no WebView**. It calls the
exact same `pollis_core::commands::*` surface the desktop app reaches over Tauri
`invoke`, but **in-process**. Ships as the `pollis` binary.

- **Crate:** `pollis-tui/` (workspace member). Binary name: `pollis`.
- **Design contract:** [`docs/pollis-tui-spec.md`](../../docs/pollis-tui-spec.md) — authoritative.
- **Status:** M0 (skeleton) + M1 (auth) implemented. M2 (read), M3 (write),
  M4 (multi-device enrollment) are the follow-on milestones in the spec.

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
| `src/main.rs` | Entry. Multi-thread tokio runtime, `Config::from_env` → `AppState::new`, terminal setup, render/input loop, panic hook. |
| `src/terminal.rs` | `TerminalGuard` — RAII enter/leave raw mode + alt screen; restores the terminal on **every** exit path (quit, `?`, panic). |
| `src/app.rs` | `App` state machine: `Screen` enum, synchronous `on_key`, async `run(Action)`. |
| `src/auth.rs` | Thin wrappers over `pollis_core::commands::{auth,pin}` that encode the M1 call order. No forked logic. |
| `src/ui.rs` | Pure `render(frame, &app)` — header / body card / status line. |

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

A fully scripted signup→send→read smoke test needs the disposable test Turso plus
a running `pollis-delivery` (the `flows` harness stands both up in-process and
forces `DEV_OTP`); reuse `.env.test` the way the harness does. Without those
credentials + a reachable DS the binary can build and start but cannot complete an
end-to-end auth round-trip.

## Milestones
- **M0** — skeleton: crate + workspace wiring, `AppState::new` boot, ratatui event
  loop, clean quit. ✅
- **M1** — auth: first-device signup (OTP→PIN→`initialize_identity`) + returning
  `get_session`→`unlock`. ✅
- **M2** — read: group/channel/DM tree, paginated message list, background poll
  loop (spec §6).
- **M3** — write: send, create group/channel, start/accept DM, invites.
- **M4** — multi-device enrollment + Secret-Key recovery UX.

See the spec for the full command-surface → screen map and the polling sync model.
