# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **Deep-dive docs:** See `.codesight/wiki/` for detailed documentation — database schemas, MLS flows, component inventory, and backend command reference. Start with `.codesight/wiki/index.md`. **Keep these docs updated** as features are developed — update the relevant wiki article alongside code changes without discussion.

> **What ships & where:** [`docs/deployments.md`](docs/deployments.md) maps every product/service (desktop app, CLI, DS, LiveKit, transparency log, pollis-verify, website) to the directory/crate it builds from, its GitHub Actions build/deploy pipeline, and where it runs. Read it to know what a change actually deploys.

## Project Overview

Pollis is a privacy-first desktop messaging app with end-to-end encryption using MLS (Message Layer Security). It's a **Tauri** app with a Rust core: the renderer (React) calls into the native Rust backend via Tauri's `invoke`, dispatched to command handlers in `src-tauri/src/commands` backed by the reusable `pollis-core` crate. Strong group encryption with Slack-style channels. The server never sees message plaintext.

> **Shell migration:** Pollis shipped on Electron through v1.x, then migrated back to Tauri (`spike/tauri-revival`, #386/#389). **Tauri is the shipping shell.** The legacy Electron shell (`electron/` + the `pollis-node` napi-rs addon) has been **removed** from the tree — it lives only in git history as a rollback reference. All work targets `src-tauri/`.

**Stack**: Tauri 2, React/TypeScript, Rust (`pollis-core` + `src-tauri`), Turso (libSQL), MLS

**Key Architecture**: The Rust backend connects **directly** to Turso (1 hop) for all CRUD. No separate backend server. The renderer invokes commands through Tauri's `invoke(cmd, args)` (via the shell-agnostic bridge in `frontend/src/bridge/invoke.ts`); `src-tauri` dispatches into command handlers that call the real implementation in `pollis-core`. Same JSON shape both ends.

## Development Commands

### Setup
```bash
pnpm install              # Install JS dependencies
```

Credentials come from `.env.development` (copy `.env.example`); the `build:tauri*` scripts source it automatically. See `docs/run-it-yourself.md` for the full credential setup.

### Running
```bash
pnpm dev                  # Builds pollis-core, then runs Vite + the Tauri shell (current; alias: dev:tauri)
pnpm dev:frontend         # Frontend only, in the browser (no Rust IPC)
```

### Building
```bash
pnpm build:tauri          # Build + bundle the Tauri app for the current platform
pnpm build:tauri:macos    # universal-apple-darwin
pnpm build:tauri:windows  # x86_64-pc-windows-msvc
pnpm build:tauri:linux    # x86_64-unknown-linux-gnu
```

Bundle config lives in `src-tauri/tauri.conf.json` (`bundle.targets: "all"`); auto-update reads the `update-{{bundle_type}}.json` manifests from `cdn.pollis.com`, with the OS code signature on each installer as trust root (Apple Developer ID, Azure Trusted Signing).

### Secrets Management

Secrets are managed via **Doppler**, which syncs to GitHub Actions secrets automatically. For local development, create a `.env.development` file manually or use Doppler CLI (`doppler run -- pnpm dev`).

### Testing

```bash
cargo test --features test-harness --test flows   # Multi-client integration tests
cargo test -p pollis --no-default-features --features test-harness --test flows   # Same suite, headless (no webkit2gtk/ALSA/dbus)
```

The integration harness (`src-tauri/tests/flows.rs`) drives the real command implementations through the same dispatch path the runtime uses — no `_inner` shims, no mocked DB layer. Each test gets its own per-client `AppState` + `InMemoryKeystore` but shares a disposable Turso instance configured in `.env.test`. See `.codesight/wiki/testing.md` for the full architecture and how to add scenarios.

## Architecture

### Network Architecture

**Rust backend (in the Tauri host process) → Turso (DIRECT libsql connection)**
- 1 network hop — simple and fast
- The Rust core has the same DB access any server would

**No separate gRPC/HTTP server** — that has been removed. All backend logic runs inside the Tauri host process, which calls `pollis-core` directly.

**The Rust core handles directly:**
- User profile CRUD
- Groups and channels CRUD
- Reading/writing to Turso
- R2 uploads/downloads
- MLS group encryption operations
- Auth (email OTP + session in OS keystore)

### Data Storage Model

**Remote Database (Turso)** — public metadata:
- Users, groups, channels, membership
- Public keys for MLS key exchange
- Encrypted message envelopes (for offline delivery)
- **Never stores**: message plaintext, private keys

**Local Database (SQLite via rusqlite)** — secrets:
- Encrypted messages (ciphertext, nonce)
- MLS group state
- **Never stores**: user profiles, groups, channels (fetched from remote)

**OS Keystore (keyring crate)**:
- Ed25519 identity key pair
- Session token

### Frontend Data Fetching

All backend calls go through the host bridge — import `invoke` from `frontend/src/bridge` (which routes to Tauri's `invoke`). Wrapped in React Query hooks:

```typescript
// React Query hooks in frontend/src/hooks/queries/
useUserProfile()                    // invoke("get_user_profile")
useUserGroups()                     // invoke("list_user_groups")
useGroupChannels(groupId)           // invoke("list_group_channels", { groupId })
useChannelMessages(channelId)       // invoke("list_messages", { channelId })
useSendMessage()                    // invoke("send_message", ...)
```

Never import directly from `@tauri-apps/*` — always go through `../bridge`. That way command call sites don't care which runtime is hosting them.

**React Query is the source of truth** for remote data — don't duplicate in the MobX store.

**MobX store**: Only holds UI state (selected group/channel), current user reference, temporary session data. Stores in `frontend/src/stores/` are MobX class singletons (`makeAutoObservable(this, …, { autoBind: true })`) — import the singleton (e.g. `import { appStore } from '../stores/appStore'`) and read fields directly inside an `observer()`-wrapped component. Non-React managers read the singleton directly and react to changes with `autorun`/`reaction`; React hooks that must stay reactive outside an `observer` use `useObserver(() => store.x)`.

### Backend Commands

Backend logic lives in `pollis-core/src/commands/` (a workspace crate with **no shell-runtime dependency** — reusable from a CLI / TUI / mobile binding). The dispatch path is `src-tauri/src/commands/<module>.rs`: a thin `#[tauri::command]` shim per command, registered in `src-tauri/src/lib.rs`'s `invoke_handler!`, that routes the JSON-shaped `invoke(cmd, args)` call from the renderer into `pollis_core::commands::*`.

**Edit `pollis-core`, not the shims.** When adding a command: implement it in `pollis-core/src/commands/<module>.rs`, add a `#[tauri::command]` shim in `src-tauri/src/commands/<module>.rs` and register it in `src-tauri/src/lib.rs`, and register it in the test harness (`src-tauri/src/test_harness.rs`) if it's covered by integration tests.

- **auth**: `initialize_identity`, `get_identity`, `request_otp`, `verify_otp`, `get_session`, `logout`
- **user**: `get_user_profile`, `update_user_profile`, `search_user_by_username`
- **groups**: `list_user_groups`, `list_group_channels`, `create_group`, `create_channel`, `invite_to_group`
- **messages**: `list_messages`, `send_message`, `poll_pending_messages`
- **mls**: MLS group key operations (the `signal/` directory only retains the MLS storage backend)
- **livekit**: `get_livekit_token`
- **r2**: `upload_file`, `download_file`

### Project Structure

```
pollis-core/            # Reusable Rust backend (no shell-runtime dependency)
  src/
    commands/           # Command implementations (auth, groups, messages, mls, voice, …)
    config.rs           # Config from env vars
    db/                 # Turso (libSQL) + local SQLite + migrations
    keystore.rs         # OS keystore (keyring)
    signal/             # MLS storage backend
    state.rs            # AppState
    realtime.rs         # LiveKit room manager + event dispatch
    sink.rs             # EventSink trait (frontend-channel abstraction)
    accounts.rs         # accounts.json (atomic, crash-safe)
    error.rs            # Error / Result types
    lib.rs              # uniffi exports for mobile bindings
src-tauri/              # Tauri desktop host — the shipping shell
  src/
    commands/           # Thin #[tauri::command] shims forwarding to pollis_core
    sink.rs             # ChannelSink adapter (Tauri's ipc::Channel → EventSink)
    test_harness.rs     # Integration-test harness (gated on feature = "test-harness")
    lib.rs              # tauri::Builder, plugins, invoke_handler!, lifecycle
    main.rs             # Binary entry
frontend/               # React app (Vite, TypeScript, TailwindCSS)
  src/
    bridge/             # Runtime-host bridge — invoke/Channel/window/dialog/fs/shell/app/updater route through Tauri's `invoke`
    hooks/queries/      # React Query hooks
    types/              # TypeScript types
    components/         # React components
    pages/              # Route pages
website/                # Static HTML marketing site (Cloudflare Pages)
```

## Media (voice / video)

**All real-time media is handled in Rust, end to end.** Voice is implemented in `pollis-core/src/commands/voice.rs` using the `livekit` + `libwebrtc` crates (capture via `cpal`, publish via `NativeAudioSource` / `LocalAudioTrack`, playback via `NativeAudioStream` → cpal output).

**Why Rust and not the renderer**: two reasons. First, **cross-platform parity** — the same media pipeline runs on desktop, and via uniffi the same `pollis-core` is consumed by mobile. One code path covers every target; the renderer's WebRTC stack would be desktop-only and would diverge from mobile's behavior on capture defaults, codec selection, and frame timing. Second, **predictable allocation** — multi-MB media buffers passed through the V8 heap create visible GC stutter; Rust's manual allocation does not. The renderer's Chromium does have full WebRTC available, but using it would mean re-implementing voice on mobile and re-introducing GC pauses. The Rust path is intentional.

Frames are pushed to the renderer over IPC channels (the frontend bridge's `channelOn`, backed by Tauri's `ipc::Channel`) for UI purposes only — speaking indicators, participant events — never for rendering media itself.

**Implication for future video**: video capture, publish, subscribe, and render are all expected to run in Rust for the same reasons. Pushing decoded frames over IPC is fine for small previews (avatars-during-call, picture-in-picture thumbnails), not for full video.

## Performance Architecture

**Lean Rust for performance-critical paths.** The Rust core exists to handle I/O, crypto, media pipelines, and concurrency without GC pauses. When a feature is performance-sensitive, IPC-bandwidth-sensitive, or benefits from no-GC predictability — media decoding, encryption, file serving, real-time pipelines, large-buffer manipulation — put it in `pollis-core`. Use the renderer as a thin presentation layer.

Don't reach for JS-side equivalents (Web Crypto, IndexedDB, browser-side caching, JS-heap byte buffers) when an equivalent Rust path exists. V8's GC pressure on multi-MB byte arrays produces visible UI stutter; Rust's predictable allocation does not. The same code also runs on mobile via uniffi bindings, so a Rust-side implementation buys cross-platform parity for free.

**Pattern for serving cached/encrypted media to the renderer**: a Rust-side local-loopback HTTP server (`127.0.0.1:<auto-port>`). The renderer embeds `<img>/<audio>/<video>` with `src="http://127.0.0.1:NNNN/<hash>"`. Rust handles disk cache (encrypted at rest), AES-GCM decrypt, HTTP Range requests, and any future on-the-fly transforms (thumbnails, transcoding, prefetch). One URL pattern across image/audio/video — no platform-branching, no custom URI schemes, no JSON IPC for bytes. CRUD continues to go through `invoke()`; the local HTTP server is for media transport, not data plane.

**Implication when adding new perf-sensitive features**: default to a Rust implementation that exposes either an `invoke()` command (for CRUD-shaped calls) or an HTTP endpoint on the local server (for byte-stream-shaped data). Reach for JS only after confirming the Rust path won't work.

## Security Model

**Trusted**: User's device, local database (encrypted at rest), the signed Tauri application binary (Tauri host + WebView renderer + `pollis-core`) at the installed version, OS keystore

**Untrusted**: Network, Turso database, server operators

**Turso can see**: User metadata, group membership, message metadata (sender, timestamp, size), connection patterns

**Turso cannot see**: Message content (encrypted), private keys (never leave device)

## Product Principles

### Backend core: invalid states are unrepresentable

**The governing principle for all `pollis-core` / remote-schema / MLS / delivery
/ retention work.** Model state so an invalid configuration *cannot be
expressed*, enforced at the lowest layer possible: **DB constraint/trigger >
Rust type > single protocol chokepoint > code discipline** (last resort, and
only with a test that encodes the invariant). A correctness property defended
only by "every caller remembers to do X" is a latent bug — if you're relying on
discipline, you modeled the state wrong.

The acceptance test we engineer for: *a member who joined a group 4 years and
300 commits ago, through dozens of adds/removals, comes back and receives every
message sent while they were a member, and catches their MLS state up to the
current epoch.* The only history ever lost is (a) messages sent before you
joined and (b) a brand-new device starting empty.

When you touch commit logs, message delivery, MLS state, or retention, your
change must make a class of bug *impossible*, and ship with a test that tries to
create the invalid state and proves it can't. "The happy path works" is not
coverage of the invariant. **Full design, failure taxonomy, and the phased
roadmap: [`docs/backend-core-invariants.md`](docs/backend-core-invariants.md).**

### Messages must work. History is bounded, not flaky.

Sending and receiving messages is the entire point of the app. Messages must not silently fail, get dropped, or become undeliverable under normal conditions. "We don't guarantee history" is **not** a license for sends to break, fail, or go invisible to a recipient who is a current member of the conversation. If something you're building can cause a message to be lost, dropped, or undecryptable for someone who was in the conversation when the message was sent, that is a bug — fix it.

The bounded-history principle means exactly two things, and nothing more:

1. **Messages sent before you joined the MLS tree are not visible to you.** If you were added to a channel/DM at epoch N, you will never see messages sent at epochs < N. That's a cryptographic property of MLS and is acceptable.
2. **New devices for an existing user don't inherit past messages.** If you add a second device, it starts empty. No history backup, no key-backup (no Megolm). Acceptable.

Everything else — delivering a message to every current member, letting the recipient decrypt it, surviving a normal offline/online cycle, showing up after the recipient accepts a pending DM request, re-syncing after a reconnect — **must work**. Unless it is cryptographically impossible or infeasible, a user should be able to read their messages from any device where they are a current member.

When designing: given the choice between "simpler model that silently drops messages" and "slightly more complex model that delivers them," **pick the one that delivers**. Simplicity stops being a virtue the moment it breaks the product's core job.

Concrete implications:
- A new device joining an existing group does not need to receive historical messages from before it joined.
- Rotating a user's identity / resetting their account may wipe prior messages on their devices, and that is acceptable.
- Do not add encrypted key-backup systems (Megolm-style) unless explicitly asked.
- But: if a user is a member of a conversation and a message was sent at an epoch they were a member of, they **must** be able to read it. Engineer for that.

## Key Files

- `src-tauri/src/main.rs` / `src-tauri/src/lib.rs` — Tauri host entry (CURRENT shell); `tauri::Builder`, plugins, `invoke_handler!`, window/tray/lifecycle
- `src-tauri/src/commands/` — Active `#[tauri::command]` dispatch — one shim per command, forwards into `pollis_core::commands::*`
- `pollis-core/src/commands/` — Real command implementations (edit here)
- `frontend/src/bridge/invoke.ts` — Shell-agnostic `invoke`/`Channel`/`listen` — routes to Tauri
- `pollis-core/src/state.rs` — AppState shared across commands
- `pollis-core/src/db/` — Turso + local SQLite + migrations
- `frontend/src/bridge/` — Runtime-host bridge — all renderer code imports `invoke`/`Channel`/window/dialog/etc. from here
- `frontend/src/main.tsx` — React app entry point
- `frontend/src/hooks/queries/` — React Query hooks
- `ARCHITECTURE.md` — Detailed architecture documentation

## Important Notes

- **Rust backend (in the Tauri host process) connects DIRECTLY to Turso** — no server middleman for CRUD
- **All backend calls from the renderer go through the bridge** — `import { invoke } from "../bridge"`, never `@tauri-apps/api/core` directly, and never `fetch()` to a local server
- **React Query is the source of truth** for remote data — don't duplicate in the MobX store
- **Local DB should NOT have users/groups/channels tables** — those come from remote Turso
- **TypeScript types should match Rust structs** — keep them synchronized
- **Remote schema changes go in numbered migration files** in `pollis-core/src/db/migrations/` (e.g. `000019_my_change.sql`). `000000_baseline.sql` is the frozen canonical snapshot — never edit it. Dev: run new migrations by hand against your dev Turso. Prod: `.github/workflows/desktop-release.yml` runs `scripts/db-apply.sh` (the `apply-migrations` job) after all builds succeed; migration failure aborts the release. Nobody applies to prod by hand. The runner records the `schema_migrations` row automatically — do **not** put an `INSERT INTO schema_migrations` in the migration file.
- **Migrations must be additive and backward-compatible with the currently-shipped desktop app.** Desktop users update on their own schedule, so after any release there will be old + new app versions hitting prod for days or weeks. Safe: `CREATE TABLE`, `ADD COLUMN` (nullable or with DEFAULT), `CREATE INDEX`, CHECK constraints already satisfied by every existing row. Unsafe — require a multi-release dance (first ship an app that stops using the thing, wait for uptake, then drop): `DROP TABLE`, `DROP COLUMN`, `RENAME`, tightening nullability or CHECKs, or any change that would make the previous app's SQL fail.
- **NEVER commit on the local `main` branch** — always create a `fix/*` or `feature/*` branch first, even if the user does not explicitly ask for one. This is absolute. If you find yourself on `main` with changes to commit, create and switch to a branch before committing.
- **Prefer editing existing files** over creating new ones
- **Always use `pnpm`** not `npm`
- **Never add Claude as a co-author on commits** — do not include `Co-Authored-By:` trailers or any Claude attribution in commit messages
- **Keep commit messages to a single line** unless the commit spans many file changes or a large body of work
- **Keep PR descriptions terse** — don't over-explain or over-detail unless the PR spans many commits or a large body of work
- **Never reinvent UI components** — always use existing components from `frontend/src/components/ui/`. Toggles/switches use `Switch`, buttons use `Button`, text inputs use `TextInput`, etc. Do not build custom styled `<button>` or `<input>` elements when a ui/ component already exists.
- **NO MODALS** — this is absolute. No fixed-position overlays, no backdrops, no dialog elements, no modal patterns of any kind. The only exception is the Cmd+K search menu. If a flow needs confirmation or input, replace the chat input bar (edit/delete bar pattern in `MainContent`) or navigate to a new page/view. A full page with two buttons is preferable to a modal.
- **Confirmation and editing flows replace the chat input bar** — render a bar in place of the chat input at the bottom of `MainContent`, following the edit/delete bar pattern already established there.
- **New static pages must be registered in three places** — when adding a page with a fixed route (e.g. `/shortcuts`, `/preferences`): (1) the route in `frontend/src/router.tsx`, (2) the `PAGE_RESULTS` array in `frontend/src/components/SearchPanel.tsx` so it's reachable via Cmd+K, and (3) the relevant nav list in `frontend/src/components/Layout/Sidebar.tsx`. This does **not** apply to dynamic/parameterized pages (e.g. `/groups/$groupId`, `/dms/$conversationId`, `/user/$userId`) — those are reached contextually, not from search/sidebar.
- **No neon / glow effects** — speaking indicators, focus rings, and other UI states use solid borders or solid backgrounds, never luminous `box-shadow` spreads or blurred accent halos.

## Design Choices

When weighing a design decision — or answering the user's question about one — look at how production apps like Slack, Discord, Linear, or other well-architected products handle the same problem. Use them as reference. Don't reinvent solved problems.

## Coding Style

### If statements always use braces
```typescript
// BAD
if (!currentUser) return;

// GOOD
if (!currentUser) {
  return;
}
```

### Component file organisation

Reusable components live in their own files. Only keep a component co-located with its parent if it is exclusively a child of that parent and will never be used elsewhere (e.g. a `ListItem` used only by `List`).

### Comments go above their relevant line, not inline
```typescript
// BAD
checkStatus(); // Verify with backend

// GOOD
// Verify with backend
checkStatus();
```

### Styling: Tailwind-first, token-backed

Design tokens are CSS custom properties in `frontend/src/index.css` (`--c-*` colors, `--bar-h`, `--font-size-base`) — the single source of truth, themeable and font-scalable. They are surfaced as semantic Tailwind utilities in `tailwind.config.js`: `bg-bg`, `bg-surface[-raised|-high]`, `text-fg`/`text-dim`/`text-muted`, `text-accent`/`bg-accent`, `border-line`/`border-line-strong`, `hover:bg-hover`, `h-bar`. **Use these utilities** — do not write `[var(--c-…)]` arbitrary classes or inline `style={{ color: 'var(--c-…)' }}` for tokens that have a utility. If a token is missing a utility, add it to the Tailwind theme rather than reaching around it.

Three idioms, in priority order:

1. **Static styling → Tailwind utilities.** The default. Colors, spacing, borders, layout. No inline `style` for static values.
2. **Runtime-dynamic values → inline `style`.** Only for values a static class cannot express: measured dimensions, dynamically-injected CSS variables (e.g. the voice meter's `--eqN`), a computed gradient stop. This is the sanctioned escape hatch.
3. **Complex / stateful / repeated selectors → a co-located component CSS file** (e.g. `voice-stage.css`, prefixed classes) or `@layer components`. For pseudo-elements, `nth-child`, `::-webkit-scrollbar`, keyframes, descendant selectors — things utilities express badly.

**Sizes that should track the user's font setting must be in `rem`** (Tailwind's scale is rem-native, so utilities scale for free; `--bar-h` is rem). Use `px` only for things that intentionally should *not* scale (1px hairlines). Never a `px` arbitrary class (`h-[28px]`) for a scalable dimension — use `rem` (`h-[1.75rem]`) or a token. Do **not** reintroduce per-file `px→rem` helpers.

Bundle size is not a factor here — Tailwind's JIT only emits used classes (CSS is a rounding error next to the JS bundle); choose for consistency, not perf.
