# Pollis-core extract — continuation notes

Tracking issue: [#223](https://github.com/actuallydan/pollis/issues/223)
Branch: `refactor/pollis-core-extract`

This file is a **temporary** working doc for finishing the refactor. Delete it before the PR merges.

## Goal recap

Move all non-Tauri-runtime backend code from `src-tauri/src/` into `pollis-core` so a future CLI / TUI / mobile / embedded-terminal binary can consume the same logic without dragging in the Tauri runtime. Frontend `invoke()` call sites must not change. Integration tests in `src-tauri/tests/flows.rs` must pass unchanged.

## Done in commits so far

The foundation modules (no Tauri runtime dependency, mechanical moves) are landed:

- `pollis-core/Cargo.toml` — full dep set (libsql, rusqlite, openmls\*, livekit, libwebrtc, cpal, keyring, reqwest, …) plus a `test-harness` feature flag.
- `pollis-core/src/error.rs` ← was `src-tauri/src/error.rs`
- `pollis-core/src/config.rs` ← was `src-tauri/src/config.rs` (incl. `Config::for_test` under `#[cfg(any(test, feature = "test-harness"))]`)
- `pollis-core/src/keystore.rs` ← was `src-tauri/src/keystore.rs` (incl. `OsKeystore`, `InMemoryKeystore`, `Keystore` trait)
- `pollis-core/src/db/{mod.rs,local.rs,remote.rs,local_schema.sql,migrations/,queries/}` ← was `src-tauri/src/db/`. New `pub const BASELINE_SQL` and `pub mod queries { pub const MESSAGES_BY_SENDER, CHANNEL_PREVIEWS }` exposed because shimmed callers can't `include_str!` across crate boundaries.
- `pollis-core/src/accounts.rs` ← was `src-tauri/src/accounts.rs`
- `pollis-core/src/signal/{mod.rs,mls_storage.rs}` ← was `src-tauri/src/signal/`

`src-tauri/src/lib.rs` re-exports each: `pub use pollis_core::config;` etc., so any code that still says `crate::error::Error` or `crate::db::local::LocalDb` keeps resolving. Existing call sites in `src-tauri/src/commands/*.rs` that did `include_str!("../db/migrations/000000_baseline.sql")` (in test modules) and `include_str!("../db/queries/*.sql")` were rewritten to `use pollis_core::db::BASELINE_SQL as BASELINE;` etc.

`pnpm dev` was NOT run after these moves — verification was `cargo check -p pollis-core` (clean) and `CXXFLAGS="-std=c++17 -w -fpermissive" cargo check -p pollis` (clean, only pre-existing warnings).

### Note on `cargo check` and webrtc-sys on macOS

`webrtc-sys` (transitively pulled by `livekit` / `libwebrtc`) fails to compile on macOS Xcode 17 SDK without `CXXFLAGS="-std=c++17 -w -fpermissive"`. `src-tauri/.cargo/config.toml` sets this when cargo is invoked from `src-tauri/`, but workspace-root invocations (`cargo check -p pollis-core`) need it set in the env. Pre-existing problem; not caused by this refactor. Either run `cd src-tauri && cargo check` or prefix with `CXXFLAGS=...`.

## What's left (the bulk)

### 1. Move command modules to `pollis-core/src/commands/`

For each file in `src-tauri/src/commands/` **except `install_kind.rs`** (it uses `tauri::utils::config::BundleType` and stays):

```
account_identity.rs   ~33 KB
auth.rs               ~55 KB
blocks.rs             ~10 KB
device_enrollment.rs  ~43 KB
dm.rs                 ~30 KB
groups.rs             ~74 KB
livekit.rs            ~45 KB  ← uses tauri::ipc::Channel + tauri::async_runtime::spawn
messages.rs           ~65 KB
mls.rs               ~149 KB  ← largest
pin.rs                ~33 KB
r2.rs                 ~21 KB
sfx.rs                ~ 4 KB
update.rs             ~ 1 KB  ← simplest, use as template
user.rs               ~15 KB
voice.rs              ~74 KB  ← uses tauri::ipc::Channel<VoiceEvent>
voice_apm.rs          ~15 KB
voice_denoiser.rs     ~ 3 KB
voice_test.rs         ~23 KB  ← uses tauri::ipc::Channel<VoiceTestEvent>
```

For each file, the transformation is:

1. Move the file: `src-tauri/src/commands/<name>.rs` → `pollis-core/src/commands/<name>.rs`.
2. Strip `#[tauri::command]` attributes from every fn.
3. Replace `state: State<'_, Arc<AppState>>` (and `tauri::State<'_, Arc<AppState>>`) with `state: &AppState`.
4. Drop `use tauri::State;`.
5. Replace `state.inner()` with `state` (it's already `&AppState`). Watch for `state.inner()` passed as `&AppState` arg — those just become `state`.
6. Replace `tauri::async_runtime::spawn` with `tokio::spawn`.
7. For `tauri::ipc::Channel<T>` parameters: see "Sink trait" section below. Replace with `Arc<dyn Sink<T>>`.
8. Update any `use crate::*` paths if needed. Most should keep working since `crate::state::AppState` is re-exported.

Then **for every `#[tauri::command]` function moved**, write a thin shim in `src-tauri/src/commands/<name>.rs`:

```rust
// src-tauri/src/commands/auth.rs (after refactor)
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
use pollis_core::commands::auth as core;

// Re-export pure types so the frontend-facing serde shapes are unchanged:
pub use pollis_core::commands::auth::{UserProfile, IdentityInfo /* … */};

#[tauri::command]
pub async fn initialize_identity(
    state: State<'_, Arc<AppState>>,
    user_id: String,
) -> Result<IdentityInfo> {
    core::initialize_identity(&state, user_id).await
}

#[tauri::command]
pub async fn request_otp(state: State<'_, Arc<AppState>>, email: String) -> Result<()> {
    core::request_otp(&state, email).await
}

// … one shim per #[tauri::command] in pollis_core::commands::auth
```

Tauri's `State<'_, Arc<AppState>>` deref-coerces to `&AppState` via `Deref<Target=Arc<AppState>>` then `Deref<Target=AppState>`, so `&state` (or `&*state` if the compiler complains) yields `&AppState`. Verify in the simplest case (`update.rs`) before scaling.

Counts of `#[tauri::command]` per file (from `grep -c "^#\[tauri::command"`):
```
account_identity:  0   (all helpers, no commands)
auth:             14
blocks:            3
device_enrollment: 9
dm:                8
groups:           23
install_kind:      1   (STAYS)
livekit:          10
messages:         14
mls:               8
pin:               4
r2:                4
sfx:               3
update:            2
user:              5
voice:            11
voice_test:        7
voice_apm:         0   (helpers only)
voice_denoiser:    0   (helpers only)
total:           ~126
```
(`lib.rs` registers ~142 entries — the delta is helper functions called from commands cross-module.)

### 2. The `Sink` trait abstraction (replaces `tauri::ipc::Channel<T>`)

Three event-channel types live in command state today:

| Where | Channel type |
|---|---|
| `realtime.rs` `LiveKitState::channel` | `Option<tauri::ipc::Channel<RealtimeEvent>>` |
| `commands/voice.rs` `VoiceState::channel` | `Option<tauri::ipc::Channel<VoiceEvent>>` |
| `commands/voice_test.rs` `VoiceTestState::channel` | `Option<tauri::ipc::Channel<VoiceTestEvent>>` |

Plus three `subscribe_*` commands take a `Channel<T>` parameter:
- `livekit::subscribe_realtime(state, channel)`
- `voice::subscribe_voice_events(state, channel)`
- `voice_test::subscribe_voice_test_events(state, channel)`

In `pollis-core` define a single small generic trait, keep it serde-tied so `RealtimeEvent`/`VoiceEvent`/`VoiceTestEvent` (which `derive(Serialize)`) work as-is:

```rust
// pollis-core/src/sink.rs (new)
use std::sync::Arc;

pub trait EventSink<T>: Send + Sync {
    fn send(&self, event: T) -> Result<(), String>;
}

// Convenience: a no-op sink the harness can use when nothing is listening.
pub struct NoopSink;
impl<T> EventSink<T> for NoopSink {
    fn send(&self, _: T) -> Result<(), String> { Ok(()) }
}
```

In each state struct: replace `Option<tauri::ipc::Channel<E>>` with `Option<Arc<dyn EventSink<E>>>`.

In `src-tauri` write one tiny adapter for Tauri's Channel:

```rust
// src-tauri/src/sink.rs (new)
use std::sync::Arc;
use serde::Serialize;
use tauri::ipc::Channel;

pub struct ChannelSink<E: Send + Sync + Clone + Serialize + 'static>(pub Channel<E>);

impl<E> pollis_core::sink::EventSink<E> for ChannelSink<E>
where
    E: Send + Sync + Clone + Serialize + 'static,
{
    fn send(&self, event: E) -> Result<(), String> {
        self.0.send(event).map_err(|e| e.to_string())
    }
}
```

Subscribe shims look like:

```rust
#[tauri::command]
pub async fn subscribe_realtime(
    state: State<'_, Arc<AppState>>,
    on_event: Channel<RealtimeEvent>,
) -> Result<()> {
    let sink = Arc::new(ChannelSink(on_event));
    pollis_core::commands::livekit::subscribe_realtime(&state, sink).await
}
```

Inside the moved code, `state.livekit.lock().await.channel = Some(channel)` becomes `… .sink = Some(sink)` and call sites that did `channel.send(event)` become `sink.send(event)`.

Two minor wrinkles to handle in `livekit.rs`:
- `dispatch_data(payload, channel)` helper — change parameter type from `&Channel<RealtimeEvent>` to `&dyn EventSink<RealtimeEvent>`.
- Two `tauri::async_runtime::spawn(async move { … })` sites — these are just `tokio::spawn`. (Tauri's async runtime IS tokio.)

### 3. Move `realtime.rs`

`pollis-core/src/realtime.rs` ← `src-tauri/src/realtime.rs`. The only change beyond move is rewriting `LiveKitState.channel` to `LiveKitState.sink` per above. `RealtimeEvent` enum is pure serde — unchanged.

`livekit::Room` and `tokio::task::JoinHandle` are already crate-agnostic; no changes there.

### 4. Move `state.rs`

`pollis-core/src/state.rs` ← `src-tauri/src/state.rs`. Once command-state types (`commands::pin::UnlockState`, `commands::voice::VoiceState`, `commands::voice_test::VoiceTestState`) and `realtime::LiveKitState` all live in `pollis-core`, this is a straight move. `AppState::new`, `new_with_parts`, `load_user_db_with_key`, `unload_user_db`, `check_not_outdated` — keep as-is.

`src-tauri/src/lib.rs` should swap `pub mod state;` for `pub use pollis_core::state;` so existing `crate::state::AppState` references keep resolving.

### 5. Update `test_harness.rs`

`src-tauri/src/test_harness.rs` stays in src-tauri (it depends on `tauri::test::MockRuntime`). Update its `use` statements:
- `use crate::config::Config;` → unchanged (still works via re-export)
- `use crate::keystore::InMemoryKeystore;` → unchanged
- The `BASELINE` const there was already updated to use `pollis_core::db::BASELINE_SQL`.

Run `CXXFLAGS="-std=c++17 -w -fpermissive" cargo test --features test-harness --test flows` and ensure it passes unchanged.

### 6. Update `src-tauri/src/lib.rs` invoke handler

The `tauri::generate_handler![…]` block currently lists `commands::auth::initialize_identity` etc. After the refactor, these names still resolve because `src-tauri/src/commands/auth.rs` exists as a shim file with the same `#[tauri::command]` functions. **No change to the `invoke_handler!` block needed.** Verify by diff.

### 7. Verification checklist

- [ ] `cargo check -p pollis-core` — clean
- [ ] `CXXFLAGS=… cargo check -p pollis` — clean
- [ ] `CXXFLAGS=… cargo test --features test-harness --test flows` — passes without modifying `tests/flows.rs`
- [ ] `pnpm dev` — app launches, can sign in, send a message, join a voice channel
- [ ] `cargo run -p pollis-core --features cli --bin uniffi-bindgen --help` — still works (note: pre-existing missing `src/bin/uniffi-bindgen.rs` may fail; unrelated to this refactor)
- [ ] `src-tauri/Cargo.toml` deps trimmed of anything `pollis-core` now owns (livekit, libwebrtc, cpal, openmls\*, libsql, rusqlite, keyring, etc.). Keep tauri + plugins + mac/linux/windows targets + `image` (used in lib.rs clipboard handler) + `url` + `serde`/`serde_json` (shims).

## Suggested commit boundaries (continuing on this branch)

The foundation moves landed in commits 1–N (already on the branch). Continue with:

- N+1 — Add `pollis-core::sink::EventSink<T>` trait + `src-tauri::sink::ChannelSink` adapter. No callers yet.
- N+2 — Move `realtime.rs` to pollis-core; rewrite `LiveKitState` to use `EventSink`.
- N+3..N+8 — Move command modules in batches matching the issue's order: simple commands (auth, user, blocks, dm, sfx, pin, update) → groups/messages/account_identity/device_enrollment/r2 → mls.rs → voice.rs/voice_apm.rs/voice_denoiser.rs → voice_test.rs → livekit.rs.
- N+9 — Move `state.rs` to pollis-core; flip `src-tauri/src/lib.rs` to `pub use pollis_core::state;`.
- N+10 — Trim `src-tauri/Cargo.toml` deps now owned by pollis-core.
- N+11 — Run `cargo test --features test-harness --test flows`, fix anything that regressed, delete this doc (`.codesight/refactoring/pollis-core-extract.md`).

## Things to watch for

- **`#[tauri::command]` macro can only sit on a real fn definition** — you can't `pub use pollis_core::commands::auth::*` and have it be invokable. Hence the shim functions in `src-tauri/src/commands/`.
- **`State<'_, Arc<AppState>>` deref**: passing `&state` into a fn that takes `&AppState` should work via two `Deref` hops. If the compiler refuses, write `&**state` (deref the State to `Arc<AppState>`, then deref that to `AppState`, then take a reference).
- **`#[cfg(test)]` mod tests inside command files**: those reference `BASELINE` etc. — already updated to use `pollis_core::db::BASELINE_SQL`. When moving the file, those imports come along.
- **`commands::auth` and `commands::mls` cross-call** (e.g. `crate::commands::mls::poll_mls_welcomes_inner(state.inner(), …)`). After move, this becomes `pollis_core::commands::mls::poll_mls_welcomes_inner(state, …)`. The `_inner` helpers (already not-`#[tauri::command]`) just move; no shim needed for them.
- **Voice DSP files** (`voice_apm.rs`, `voice_denoiser.rs`) have no `#[tauri::command]` functions — they're pure helpers consumed by `voice.rs`. Move with no shim.
- **`account_identity.rs`** also has no `#[tauri::command]` functions — pure helpers consumed by `auth.rs` / `device_enrollment.rs`. Move with no shim.
- **Don't forget `src-tauri/src/commands/mod.rs`** — currently lists every command module. After refactor it should match the new shim-only set (drop `voice_apm`, `voice_denoiser`, `account_identity` if they live only in pollis-core; keep them as `pub mod` in pollis-core's commands).
