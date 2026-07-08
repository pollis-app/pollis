# Contributing to Pollis

This is the developer guide: how to build, run, and test Pollis locally, and the
conventions a change is expected to follow. If you are here to **use** the app or
to **verify** its privacy claims as an auditor, start with the [README](README.md)
instead — this file assumes you intend to run the source.

For the architecture (command path, storage split, MLS flows), read
[`CLAUDE.md`](CLAUDE.md) and [`ARCHITECTURE.md`](ARCHITECTURE.md); the full
subsystem docs live in [`.codesight/wiki/`](.codesight/wiki/index.md).

## Prerequisites

- Node.js 18+
- pnpm 10.25+
- Rust (stable, via rustup)
- Tauri system dependencies for your OS — see <https://v2.tauri.app/start/prerequisites/>
  (e.g. `webkit2gtk` + `build-essential` on Linux, Xcode Command Line Tools on
  macOS, MSVC Build Tools on Windows)
- Credentials for the backing services. The quickest path is Doppler access (ask
  the project owner). To stand up **your own** Turso DB, LiveKit SFU, and R2
  bucket and run the real client end to end against infrastructure you control,
  follow [docs/run-it-yourself.md](docs/run-it-yourself.md).

## Setup

```bash
pnpm install          # install JS dependencies
```

## Running

```bash
pnpm dev              # full desktop app — builds pollis-core, then runs Vite + the Tauri shell
pnpm dev:frontend     # frontend only, in the browser (no Rust IPC)
pnpm dev:cli          # pollis-tui terminal client
```

`pnpm dev` (alias `pnpm dev:tauri`) starts the Vite dev server on `:5173` and the
Tauri host in parallel; the host compiles `src-tauri` (and the `pollis-core`
crates it depends on) on first run, which can take a few minutes, then is fast.
The Linux invocation sets `WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11` for
WebKitGTK compatibility (already baked into the script).

### Skipping email OTP in development

Add `DEV_OTP=000000` to `.env.development`. With this set, hitting "Continue" on
the login screen skips the Resend email call and stores a hash of `000000` as the
valid code — type it in the OTP field to sign in. The session persists to the OS
keystore so you only need to do this once per fresh install.

For fully hands-free startup, set `DEV_EMAIL=you@example.com` instead. This
bypasses OTP entirely and auto-logs in as that email on every launch (creating the
user in Turso if needed).

### Testing with two users

```bash
# Terminal 1 — user A
pnpm dev

# Terminal 2 — user B
POLLIS_DATA_DIR=/tmp/pollis-dev2 pnpm dev
```

`POLLIS_DATA_DIR` gives the second instance its own local SQLite database and
keystore, so the two instances don't interfere. Both hit the same Turso database,
so messages appear in real time across windows via LiveKit.

### Testing multi-device (same user, two devices)

```bash
# Terminal 1 — device 1
DEV_EMAIL=you@example.com pnpm dev

# Terminal 2 — device 2
DEV_EMAIL=you@example.com POLLIS_DATA_DIR=/tmp/pollis-dev2 pnpm dev
```

Both instances log in as the same user, but `POLLIS_DATA_DIR` isolates the
keystore and local DB so each gets its own `device_id` and MLS state — they
register as separate devices in the `user_device` table. Messages sent from a
third user (or from either device) should appear on both.

### Development environment variables

All dev-only env vars. Set them in `.env.development` or pass inline.

| Variable | Purpose |
|----------|---------|
| `DEV_OTP` | Fixed OTP code (e.g. `000000`) — skips Resend email, accepts this code on the OTP screen |
| `DEV_EMAIL` | Auto-login as this email on startup — bypasses OTP entirely |
| `POLLIS_DATA_DIR` | Override the local data directory — isolates local DB and keystore for running multiple instances |

## Building

```bash
# Build + bundle the Tauri app for the current platform
pnpm build:tauri

# Bundle for a specific target
pnpm build:tauri:macos     # universal-apple-darwin
pnpm build:tauri:windows   # x86_64-pc-windows-msvc
pnpm build:tauri:linux     # x86_64-unknown-linux-gnu
```

The bundle config lives in `src-tauri/tauri.conf.json` (`bundle.targets: "all"`).
The `build:tauri*` scripts source `.env.development` so the compiled-in
credentials are present. Local builds skip code signing unless the platform
signing env vars are set — CI sets all of them.

## Testing

| Command | What runs |
|---|---|
| `cargo test` | Unit tests only (in-crate `#[cfg(test)]` modules) |
| `cargo test --features test-harness` | Unit tests + multi-client integration harness |
| `cargo test --all-features` | Same as above — `--all-features` turns on `test-harness` |
| `cargo test --features test-harness --test flows` | Integration harness only |
| `cargo test -p pollis-delivery` | Delivery Service endpoint tests |

The integration harness (`src-tauri/tests/flows.rs`) is gated behind the
`test-harness` Cargo feature because it takes ~3–4 minutes, serializes on a
process-wide mutex, and requires a disposable Turso database configured in
`.env.test` at the repo root. On a headless box, build the core without the
desktop shell: `-p pollis --no-default-features --features test-harness`. See
[`.codesight/wiki/testing.md`](.codesight/wiki/testing.md) for the full
architecture.

## Conventions

These are enforced in review; a PR that ignores them will be sent back.

- **Never commit on `main`.** Always branch first — `fix/*` or `feature/*`.
- **Always `pnpm`, never `npm`.** Prefer editing existing files over creating new
  ones.
- **Update the docs alongside the code.** A change to a subsystem updates the
  relevant `.codesight/wiki/` article in the same PR, without being asked.
- **Invalid states unrepresentable.** Enforce invariants at the lowest layer
  (DB constraint > Rust type > protocol chokepoint > code discipline), and ship a
  test that proves the invalid state can't be created — "happy path works" is not
  coverage. See [`docs/backend-core-invariants.md`](docs/backend-core-invariants.md).
- **Remote schema changes are numbered, additive migrations** in
  `pollis-core/src/db/migrations/` — never edit the baseline, never hand-insert
  into `schema_migrations`, and keep every migration backward-compatible with the
  shipped app.
- **Keep the TypeScript types in sync with the Rust structs** across the `invoke`
  bridge.
- **Commit style:** single-line messages, terse PR descriptions unless the scope
  is large. No AI attribution trailers.

The renderer talks to the backend only through `frontend/src/bridge` (never
`@tauri-apps/*` directly, never `fetch()` to a local server), and remote **writes**
always go through a Delivery Service endpoint — never a client-side INSERT/UPDATE
against Turso. The full rules live in [`CLAUDE.md`](CLAUDE.md).
