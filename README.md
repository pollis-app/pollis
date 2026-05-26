# Pollis

A desktop messaging app with end-to-end encryption. Think Slack, but nobody — including the people running it — can read your messages. Built as an Electron app on macOS, Linux, and Windows, with a React frontend and a native Rust backend loaded into Electron's main process as a Node addon — the heavy lifting (crypto, MLS state, voice/screenshare) runs in Rust, not in the renderer.

![Pollis App](readme/hero.png)

## How it works

Messages are encrypted on your device using MLS (Messaging Layer Security) before they ever leave your machine. The Rust backend connects directly to Turso (libSQL) for group and channel metadata. There is no intermediate server — the desktop binary is the backend. Encrypted message envelopes are stored remotely for offline delivery, and decrypted message history lives in a local SQLite database encrypted at rest.

The renderer calls into the Rust backend via a single IPC bridge: `window.electronAPI.invoke(cmd, args)` → preload → Electron main process → `pollis-node` (the napi-rs binding that loads `pollis-core` as a native Node addon) → real implementation in `pollis-core`. Same JSON shape on both ends.

**Stack**
- **Desktop shell**: Electron 33 (main process + Chromium renderer + preload bridge)
- **Frontend**: React 19, TypeScript, Vite, TailwindCSS
- **Backend**: Rust split into `pollis-core` (reusable crate; also consumed by mobile via uniffi) and `pollis-node` (napi-rs binding loaded into the Electron main process)
- **Encryption**: MLS (Messaging Layer Security) for group channel encryption, AES-256-GCM. Voice channels are end-to-end encrypted too — per-frame AES-128-GCM via libwebrtc's `FrameCryptor`, keyed from the MLS group's exporter secret, so the LiveKit SFU forwards ciphertext only.
- **Remote DB**: Turso (libSQL) — direct from the Rust core, no middleman
- **Local DB**: SQLite via rusqlite (encrypted at rest, key in OS keystore)
- **Auth**: Email OTP, session stored in the OS keystore
- **Real-time**: LiveKit (voice calls via Rust `livekit` crate, real-time presence)
- **File storage**: Cloudflare R2

## Security model

Message content, file attachments, and voice audio are encrypted on your device before they ever leave it. The server stores ciphertext it can never read — your messages, files, and voice calls are inaccessible to anyone operating the infrastructure. Private keys never leave the device. Session tokens live in the OS keystore (macOS Keychain, Windows Credential Manager, Linux Secret Service), not on disk.

Voice channels add a second layer of encryption on top of the standard DTLS-SRTP that protects the link to LiveKit. Each audio frame is AES-128-GCM encrypted by libwebrtc's `FrameCryptor` after Opus compression and before SRTP, keyed by a 32-byte secret derived from the channel's MLS group via `MlsGroup::export_secret`. The SFU routes RTP packets without being able to decode the payloads, the key rotates on every MLS epoch advance, and the design matches what `livekit-client` exposes as `setupE2EE` and what Discord ships as DAVE.

Forward secrecy is provided by MLS's key schedule: each epoch advance rotates the group key material, and each message uses a unique derived key so compromising one doesn't expose past or future messages.

## Releases

Builds for macOS, Windows, and Linux are published automatically on every version tag via GitHub Actions (`.github/workflows/electron-release.yml`). `electron-builder` produces DMG + ZIP (mac), NSIS + portable (win), and AppImage + deb + rpm (linux); the workflow uploads them as GitHub Release assets along with the `latest.yml` / `latest-mac.yml` / `latest-linux.yml` manifests that `electron-updater` reads at runtime. The same workflow mirrors a `latest.json` to R2 so the marketing site at [pollis.com](https://pollis.com) can always show the current download links. Auto-update trust is rooted in the OS code signature on each installer — Apple Developer ID + notarization on macOS, Azure Trusted Signing on Windows.

![Pollis UI](readme/new_app.png)

## Getting started

### Prerequisites

- Node.js 18+
- pnpm 10.25+
- Rust (stable, via rustup)
- A C/C++ toolchain so `napi-rs` can link the `pollis-node` addon: Xcode Command Line Tools on macOS, MSVC Build Tools on Windows, `build-essential` on Linux
- Access to Doppler for secrets (ask the project owner)

### Setup

```bash
pnpm install          # Install JS dependencies
```

### Running

```bash
pnpm dev              # Full desktop app — builds pollis-node (debug), then runs Vite + Electron concurrently
pnpm dev:frontend     # Frontend only, in the browser (no Electron main process, no Rust IPC)
```

Under the hood `pnpm dev` builds the Rust addon with `pnpm --filter @pollis/pollis-node build-debug`, then starts the Vite dev server on `:5173` and Electron's main process in parallel via `concurrently`. The first run also builds dependent Rust crates, which can take a few minutes; subsequent runs are fast.

### Skipping email OTP in development

Add `DEV_OTP=000000` to `.env.development`. With this set, hitting "Continue" on the login screen skips the Resend email call and stores a hash of `000000` as the valid code — type it in the OTP field to sign in. The session persists to the OS keystore so you only need to do this once per fresh install.

For fully hands-free startup, set `DEV_EMAIL=you@example.com` instead. This bypasses OTP entirely and auto-logs in as that email on every launch (creating the user in Turso if needed).

### Testing with two users

```bash
# Terminal 1 — user A
pnpm dev

# Terminal 2 — user B
POLLIS_DATA_DIR=/tmp/pollis-dev2 pnpm dev
```

`POLLIS_DATA_DIR` gives the second instance its own local SQLite database and keystore, so the two instances don't interfere. Both hit the same Turso database, so messages appear in real time across windows via LiveKit.

### Testing multi-device (same user, two devices)

```bash
# Terminal 1 — device 1
DEV_EMAIL=you@example.com pnpm dev

# Terminal 2 — device 2
DEV_EMAIL=you@example.com POLLIS_DATA_DIR=/tmp/pollis-dev2 pnpm dev
```

Both instances log in as the same user, but `POLLIS_DATA_DIR` isolates the keystore and local DB so each gets its own `device_id` and MLS state — they register as separate devices in the `user_device` table. Messages sent from a third user (or from either device) should appear on both.

### Development environment variables

All dev-only env vars. Set them in `.env.development` or pass inline.

| Variable | Purpose |
|----------|---------|
| `DEV_OTP` | Fixed OTP code (e.g. `000000`) — skips Resend email, accepts this code on the OTP screen |
| `DEV_EMAIL` | Auto-login as this email on startup — bypasses OTP entirely |
| `POLLIS_DATA_DIR` | Override the local data directory — isolates local DB and keystore for running multiple instances |

### Building

```bash
# Build the Rust addon (release) and the Electron main bundle
pnpm build:pollis-node
pnpm --filter @pollis/electron build

# Package for the current platform
pnpm --filter @pollis/electron exec electron-builder --config build/electron-builder.yml

# Package for a specific target
pnpm --filter @pollis/electron exec electron-builder --config build/electron-builder.yml --mac
pnpm --filter @pollis/electron exec electron-builder --config build/electron-builder.yml --win
pnpm --filter @pollis/electron exec electron-builder --config build/electron-builder.yml --linux
```

The packaging config lives at `electron/build/electron-builder.yml`. Local builds skip code signing unless the relevant env vars (`CSC_LINK` / `CSC_KEY_PASSWORD` on macOS; `SIGNTOOL_PATH` / `SIGNING_DLIB_PATH` / `SIGN_METADATA_PATH` on Windows) are set — CI sets all of them.

#### Legacy Tauri build (rollback only)

The Tauri shell at `src-tauri/` is retained as a rollback path, not the active build. The `dev:tauri` / `build:tauri*` scripts in the root `package.json` still work, but Tauri is not what ships.

### Testing

| Command | What runs |
|---|---|
| `cargo test` | Unit tests only (in-crate `#[cfg(test)]` modules) |
| `cargo test --features test-harness` | Unit tests + multi-client integration harness |
| `cargo test --all-features` | Same as above — `--all-features` turns on `test-harness` |
| `cargo test --features test-harness --test flows` | Integration harness only |

The integration harness (`src-tauri/tests/flows.rs`) is gated behind the `test-harness` Cargo feature because it takes ~3–4 minutes, serializes on a process-wide mutex, and requires a disposable Turso database configured in `.env.test` at the repo root. See [`.codesight/wiki/testing.md`](./.codesight/wiki/testing.md) for the full architecture.

## Project layout

```
pollis-core/  # Reusable Rust backend — commands, DB, MLS encryption, auth (no shell dependency; also exposed to mobile via uniffi)
pollis-node/  # napi-rs binding — loads pollis-core into Node, dispatches `invoke` calls from the Electron main process
electron/     # Electron app — main process, preload bridge, electron-builder config
frontend/    # React app — Vite, TypeScript, TailwindCSS, runtime-host bridge at src/bridge/
src-tauri/   # Legacy Tauri desktop binary — retained for rollback, not the active shell
website/     # Static marketing site — plain HTML/CSS/JS, deployed to Cloudflare Pages
```

## What's coming

- **Broader platform availability** — currently open pre-alpha; working toward a stable public release
