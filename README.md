# Pollis

A desktop messaging app with end-to-end encryption. Think Slack, but nobody — including the people running it — can read your messages. Built as a Tauri app on macOS, Linux, and Windows, with a React frontend and a native Rust backend — the heavy lifting (crypto, MLS state, voice/screenshare) runs in Rust, not in the renderer.

> **Want to verify the privacy claims yourself?** The repo is public and the app runs entirely on infrastructure you control — see [docs/run-it-yourself.md](docs/run-it-yourself.md) to stand up your own Turso DB, LiveKit SFU, and R2 bucket and run the real client end to end.

![Pollis App](readme/hero.png)

## How it works

Messages are encrypted on your device using MLS (Messaging Layer Security) before they ever leave your machine. The Rust backend connects directly to Turso (libSQL) for group and channel metadata. There is no intermediate server — the desktop binary is the backend. Encrypted message envelopes are stored remotely for offline delivery, and decrypted message history lives in a local SQLite database encrypted at rest.

The renderer calls into the Rust backend via a single IPC bridge: Tauri's `invoke(cmd, args)` (through the shell-agnostic wrapper at `frontend/src/bridge/invoke.ts`) → `src-tauri` command dispatch → real implementation in `pollis-core`. Same JSON shape on both ends.

**Stack**
- **Desktop shell**: Tauri 2 (Rust host + system WebView renderer)
- **Frontend**: React 19, TypeScript, Vite, TailwindCSS
- **Backend**: Rust split into `pollis-core` (reusable crate; also consumed by mobile via uniffi) and `src-tauri` (the Tauri host that exposes `pollis-core` to the renderer via `invoke`).
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

Every MLS commit is also published to an append-only **transparency log** so the server can't quietly fork, roll back, or rewrite a conversation's history without detection. You can verify it yourself, trusting only the log's signed public key — see [docs/transparency.md](docs/transparency.md) and the step-by-step [verify-it-yourself guide](docs/verify-transparency-log.md).

## Releases

Builds for macOS, Windows, and Linux are published automatically on every version tag via the Tauri release workflow (re-armed on tag push in #386). Tauri's bundler produces the platform installers and the `update-{{bundle_type}}.json` manifests that the in-app updater reads at runtime from `cdn.pollis.com`, so the marketing site at [pollis.com](https://pollis.com) always shows the current download links. Auto-update trust is rooted in the OS code signature on each installer — Apple Developer ID + notarization on macOS, Azure Trusted Signing on Windows.

![Pollis UI](readme/new_app.png)

## Getting started

### Prerequisites

- Node.js 18+
- pnpm 10.25+
- Rust (stable, via rustup)
- Tauri system dependencies for your OS — see <https://v2.tauri.app/start/prerequisites/> (e.g. `webkit2gtk` + `build-essential` on Linux, Xcode Command Line Tools on macOS, MSVC Build Tools on Windows)
- Access to Doppler for secrets (ask the project owner), or provide your own — see [docs/run-it-yourself.md](docs/run-it-yourself.md)

### Setup

```bash
pnpm install          # Install JS dependencies
```

### Running

```bash
pnpm dev              # Full desktop app — builds pollis-core, then runs Vite + the Tauri shell
pnpm dev:frontend     # Frontend only, in the browser (no Rust IPC)
```

`pnpm dev` (alias `pnpm dev:tauri`) starts the Vite dev server on `:5173` and the Tauri host in parallel; the host compiles `src-tauri` (and the `pollis-core` crates it depends on) on first run, which can take a few minutes, then is fast. The Linux invocation sets `WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11` for WebKitGTK compatibility (already baked into the script).

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
# Build + bundle the Tauri app for the current platform
pnpm build:tauri

# Bundle for a specific target
pnpm build:tauri:macos     # universal-apple-darwin
pnpm build:tauri:windows   # x86_64-pc-windows-msvc
pnpm build:tauri:linux     # x86_64-unknown-linux-gnu
```

The bundle config lives in `src-tauri/tauri.conf.json` (`bundle.targets: "all"`). The `build:tauri*` scripts source `.env.development` so the compiled-in credentials are present. Local builds skip code signing unless the platform signing env vars are set — CI sets all of them.

### Testing

| Command | What runs |
|---|---|
| `cargo test` | Unit tests only (in-crate `#[cfg(test)]` modules) |
| `cargo test --features test-harness` | Unit tests + multi-client integration harness |
| `cargo test --all-features` | Same as above — `--all-features` turns on `test-harness` |
| `cargo test --features test-harness --test flows` | Integration harness only |

The integration harness (`src-tauri/tests/flows.rs`) is gated behind the `test-harness` Cargo feature because it takes ~3–4 minutes, serializes on a process-wide mutex, and requires a disposable Turso database configured in `.env.test` at the repo root. See [`.codesight/wiki/testing.md`](./.codesight/wiki/testing.md) for the full architecture.

## What this repo produces

This is a monorepo. Despite the desktop app being the headline, it ships a number of distinct, independently-deployable artifacts:

| Output | Lives in | What it is |
|---|---|---|
| **Desktop app** | `src-tauri/` + `frontend/` | The Tauri client, bundled for macOS / Windows / Linux |
| **Mobile app** | `mobile/` | React Native / Expo client for iOS + Android (consumes `pollis-core` via uniffi; in development) |
| **MLS Delivery Service** | `pollis-delivery/` | Dockerized axum service — the sole writer that serializes MLS commits server-side (`api.pollis.com`); crypto stays client-side |
| **LiveKit stack** | `livekit/` | docker-compose + nginx config for the self-hostable voice/screenshare SFU |
| **Transparency publisher** | `verifiable-log-serve/` | Dockerized builder/serve that publishes the signed append-only log to R2 (`verify.pollis.com`) |
| **Website** | `website/` | Static marketing + docs site (Cloudflare Pages), including the transparency explorer |
| **CLI tools** | `verifiable-log*/` | `pollis-verify` (public log verifier), plus the lower-level `monitor`, `builder`, and `serve` binaries |
| **AUR package** | `aur/` | `PKGBUILD` for Arch Linux distribution of the desktop app |
| **The transparency log itself** | _(scheduled output)_ | The daily signed Merkle log synced to R2 — the verifiable artifact the whole transparency system exists to produce |

The reusable backend (`pollis-core`) is the shared spine: the desktop host, the mobile bindings, and the CLIs all build on it.

## Project layout

```
pollis-core/      # Reusable Rust backend — commands, DB, MLS encryption, auth (no shell dependency; also exposed to mobile via uniffi)
src-tauri/        # Tauri desktop host (current shell) — commands, tray, window lifecycle; exposes pollis-core via `invoke`
frontend/         # React app — Vite, TypeScript, TailwindCSS, runtime-host bridge at src/bridge/
mobile/           # React Native / Expo client (iOS + Android)
pollis-delivery/  # MLS Delivery Service — axum, sole writer that serializes commits server-side
verifiable-log*/  # Transparency log core, builder, serve, and the pollis-verify CLI
livekit/          # Self-host config for the LiveKit SFU (docker-compose + nginx)
website/          # Static marketing site — plain HTML/CSS/JS, deployed to Cloudflare Pages
```

## What's coming

- **Broader platform availability** — currently open pre-alpha; working toward a stable public release
