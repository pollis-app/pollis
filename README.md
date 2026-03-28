# Pollis

A desktop messaging app with end-to-end encryption. Think Slack, but nobody — including the people running it — can read your messages. Built with Tauri 2, so it's a native app on macOS, Linux, and Windows with a Rust backend and React frontend.

![Pollis App](readme/hero.png)

## How it works

Messages are encrypted on your device using the Signal protocol before they ever leave your machine. The backend connects directly to Turso (libSQL) for group and channel metadata. There is no intermediate server — the Tauri app is the backend. Encrypted message envelopes are stored remotely for offline delivery, and decrypted message history lives in a local SQLite database encrypted at rest.

**Stack**
- **Desktop**: Tauri 2 (Rust + React/TypeScript)
- **Encryption**: Signal Protocol — X3DH key exchange, Double Ratchet, AES-256-GCM
- **Remote DB**: Turso (libSQL) — direct from the app, no middleman
- **Local DB**: SQLite via SQLCipher (encrypted at rest)
- **Auth**: Email OTP, session stored in the OS keystore
- **Real-time**: LiveKit (WebRTC data channels for message delivery)
- **File storage**: Cloudflare R2

## Security model

The server only ever sees encrypted blobs. Turso stores group metadata, public keys, and ciphertext — never message content or private keys. Private keys never leave the device. Session tokens live in the OS keystore (macOS Keychain, Windows Credential Manager, Linux Secret Service), not on disk.

Forward secrecy is provided by the Double Ratchet: each message uses a unique derived key, so compromising one doesn't expose past messages.

## Releases

Builds for macOS (Universal), Windows, and Linux are published automatically on every version tag via GitHub Actions. Binaries are uploaded to Cloudflare R2, and a `latest.json` manifest is written alongside them. The marketing site at [pollis.com](https://pollis.com) reads that manifest on load to always show the current download links.

![Pollis UI](readme/new_app.png)

## Getting started

### Prerequisites

- Node.js 18+
- pnpm 10.25+
- Rust (stable, via rustup)
- Tauri v2 system dependencies — see [tauri.app/start/prerequisites](https://tauri.app/start/prerequisites/)
- Access to Doppler for secrets (ask the project owner)

### Setup

```bash
pnpm install          # Install JS dependencies
```

### Running

```bash
pnpm dev              # Full desktop app (Rust + React)
pnpm dev:frontend     # Frontend only in browser (no Tauri commands)
```

### Skipping email OTP in development

Add `DEV_OTP=000000` to `.env.development`. With this set, hitting "Continue" on the login screen skips the Resend email call and stores a hash of `000000` as the valid code — type it in the OTP field to sign in. The session persists to the OS keystore so you only need to do this once per fresh install.

### Testing with two users

```bash
# Terminal 1 — user A
pnpm dev

# Terminal 2 — user B
POLLIS_DATA_DIR=/tmp/pollis2 pnpm dev
```

Both instances hit the same Turso database, so messages appear in real time across windows via LiveKit.

### Building

```bash
pnpm build            # Current platform
pnpm build:macos      # Universal binary (Intel + Apple Silicon)
pnpm build:linux      # amd64 AppImage
pnpm build:windows    # amd64 NSIS installer
```

## Project layout

```
src-tauri/   # Rust backend — Tauri commands, DB, Signal protocol, auth
frontend/    # React app — Vite, TypeScript, TailwindCSS
website/     # Static marketing site — plain HTML/CSS/JS, deployed to Cloudflare Pages
```

## What's coming

- **Auto-update** — in-app update prompts and one-click installs via the Tauri updater plugin, driven by the `latest.json` manifest already published on each release
- **Voice and video calls** — LiveKit is already integrated for real-time messaging; call support is the natural next step
- **macOS code signing and notarization** — so Gatekeeper stops complaining
- **Broader platform availability** — currently open pre-alpha; working toward a stable public release
