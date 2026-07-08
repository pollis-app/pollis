# Pollis

A desktop messaging app with end-to-end encryption. Think Slack, but nobody —
including the people running it — can read your messages. Built as a Tauri app on
macOS, Linux, and Windows, with a React frontend and a native Rust backend — the
heavy lifting (crypto, MLS state, voice/screenshare) runs in Rust, not in the
renderer.

**Download:** [pollis.com](https://pollis.com) always links the current signed
build for each platform.
**Run or verify it yourself:** you don't have to take the privacy claims on faith
— see [Verify it yourself](#verify-it-yourself) below.
**Want to contribute?** Build/run/test instructions live in
[CONTRIBUTING.md](CONTRIBUTING.md).

![Pollis App](readme/hero.png)

## How it works

Messages are encrypted on your device using MLS (Messaging Layer Security) before
they ever leave your machine. The Rust backend connects directly to Turso
(libSQL) for group and channel metadata. There is no intermediate server — the
desktop binary is the backend. Encrypted message envelopes are stored remotely for
offline delivery, and decrypted message history lives in a local SQLite database
encrypted at rest.

**Stack**
- **Desktop shell**: Tauri 2 (Rust host + system WebView renderer)
- **Frontend**: React 19, TypeScript, Vite, TailwindCSS
- **Backend**: Rust split into `pollis-core` (reusable crate; also consumed by
  mobile via uniffi) and `src-tauri` (the Tauri host that exposes `pollis-core`
  to the renderer via `invoke`).
- **Encryption**: MLS for group channel encryption, AES-256-GCM. Voice channels
  are end-to-end encrypted too — per-frame AES-128-GCM via libwebrtc's
  `FrameCryptor`, keyed from the MLS group's exporter secret, so the LiveKit SFU
  forwards ciphertext only.
- **Remote DB**: Turso (libSQL) — direct from the Rust core, no middleman
- **Local DB**: SQLite via rusqlite (encrypted at rest, key in OS keystore)
- **Auth**: Email OTP, session stored in the OS keystore
- **Real-time**: LiveKit (voice calls via Rust `livekit` crate, real-time presence)
- **File storage**: Cloudflare R2

## Security model

Message content, file attachments, and voice audio are encrypted on your device
before they ever leave it. The server stores ciphertext it can never read — your
messages, files, and voice calls are inaccessible to anyone operating the
infrastructure. Private keys never leave the device. Session tokens live in the OS
keystore (macOS Keychain, Windows Credential Manager, Linux Secret Service), not
on disk.

- **What the server can never see:** message text, file contents, and voice audio.
  It only ever holds ciphertext and public metadata (who is in a channel, when
  commits happened).
- **Forward secrecy:** MLS's key schedule rotates the group key on every epoch
  advance, and each message uses a unique derived key, so compromising one key
  doesn't expose past or future messages.
- **Voice:** a second encryption layer sits on top of the standard DTLS-SRTP link
  to LiveKit. Each Opus frame is AES-128-GCM encrypted by libwebrtc's
  `FrameCryptor` before SRTP, keyed by a 32-byte secret derived from the channel's
  MLS group via `MlsGroup::export_secret`; the key rotates on every MLS epoch. The
  SFU routes packets it cannot decode. The design matches what `livekit-client`
  exposes as `setupE2EE` and what Discord ships as DAVE.
- **Tamper-evident history:** every MLS commit is published to an append-only
  **transparency log**, so the server can't quietly fork, roll back, or rewrite a
  conversation's history without detection.

**Honest scope.** Pollis hides message *content*, not the *fact* that you are
communicating. The server and network still see connection metadata (IP address,
timing, which accounts are in a channel). There is no sender anonymity, no
IP-hiding relay (an optional overlay is designed but deferred —
[docs/relay-overlay-design.md](docs/relay-overlay-design.md)), and the key
exchange is classical X25519, not yet post-quantum (planned —
[docs/pq-hybrid-mls-design.md](docs/pq-hybrid-mls-design.md)). The full,
caveated threat model is in
[docs/security-whitepaper.md](docs/security-whitepaper.md); a plain-language
version is [docs/security-simple.md](docs/security-simple.md).

## Verify it yourself

The point of Pollis is that you don't have to trust the operator — you can check
the claims. Three independent, credential-free ways to do that, in increasing
depth:

### 1. Audit the transparency log with the `pollis-verify` CLI

Pollis publishes an append-only Merkle log (RFC 6962 / RFC 9162, the same
construction Certificate Transparency uses) covering **three** trees: every MLS
commit, every account identity-key version, and every released binary. The
verifier trusts **only** the log's published Ed25519 public key — not the server,
the database, or the host serving the files. If a single byte is tampered with, a
signature or proof check fails and the tool exits non-zero.

Pinned public key:
`175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148`

Grab a prebuilt `pollis-verify` from the
[Releases](https://github.com/actuallydan/pollis/releases) page (tags
`pollis-verify-v*`; the pinned key is printed in the release body) or build it
from source with `cargo build -p verifiable-log-serve --release`. Then:

```bash
# Verify the WHOLE log end to end — every STH signature, every entry replay,
# every inclusion + consistency proof, no equivocation across heads.
pollis-verify remote https://verify.pollis.com

# Verify one conversation's commit chain is provably included and fork-free.
pollis-verify group   https://verify.pollis.com <conversation-id>

# Verify one user's account-key history is append-only (no silent key swap).
pollis-verify account https://verify.pollis.com <user-id>

# Verify a released build's logged hashes match a given release tag.
pollis-verify release https://verify.pollis.com <tag>
```

A passing run prints a `PASS` line per check and exits `0`; any failure prints
`FAIL` and exits non-zero — and that exit code is computed from the signature and
the proofs, not from anything the server told you to believe. The desktop app runs
the *same* `account` verifier internally to self-audit its own identity key, so
the client and an independent auditor reach an identical verdict.

Prefer to trust nothing on the network during verification? Download the signed
bundle once and check it fully offline with `monitor verify <bundle.json>`
(`cargo build -p verifiable-log --release`). The step-by-step walkthrough,
including sample output for every subcommand, is
[docs/verify-transparency-log.md](docs/verify-transparency-log.md).

### 2. Read the verification API directly

The log is a plain, immutable, unauthenticated static read API under
`https://verify.pollis.com/v1/` — you can `curl` it and check the math with your
own tooling. The verifier above is just a convenient client for these bytes.

| Path | What it is |
|---|---|
| `/v1/public_key.json` | the log's Ed25519 public key |
| `/v1/sth/latest.json` | newest Signed Tree Head (`tree_size`, `root_hash`, `timestamp`, signature) |
| `/v1/sth/<tree_size>.json` | the immutable STH at that size |
| `/v1/entries.json` · `/v1/entries/<i>.json` | the full ordered log, and each leaf |
| `/v1/proof/inclusion/<size>/<leaf>.json` | inclusion proof |
| `/v1/proof/consistency/<a>-<b>.json` | append-only consistency proof between two heads |

The account-key and binaries trees mirror this exact layout under
`/v1/account-keys/...` and `/v1/binaries/...`, each domain-separated so an STH
minted for one tree can't be replayed as another's. The browser explorer at
[pollis.com/transparency](https://pollis.com/transparency) is a convenience
front-end over the same data — it is exactly as trustworthy as the server hosting
it, which is why the trustworthy path is running the CLI yourself. Full API
reference: [docs/transparency.md](docs/transparency.md).

### 3. Read the publish-and-self-audit run logs

The log isn't just published — every publish **re-verifies what was actually
served** and runs an across-run equivocation tripwire, in public CI. The
[`transparency-publish`](https://github.com/actuallydan/pollis/actions/workflows/transparency-publish.yml)
workflow builds the signed bundles, syncs them to R2, then runs
`pollis-verify remote` against the live `verify.pollis.com` and compares the new
heads against a cached copy of the previous run's — so an equivocating or
rolled-back head trips the build. Open any run on the Actions tab and read the
"final verify" step: a green run is a signed, timestamped, public record that the
served log verified against its own pinned key. Release builds are logged the same
way by the desktop-release pipeline, which appends each artifact's hashes to the
binaries tree.

### 4. Run the whole thing yourself

The repo is public and the app runs entirely on infrastructure you control.
[docs/run-it-yourself.md](docs/run-it-yourself.md) walks through standing up your
own Turso DB, LiveKit SFU, and R2 bucket and running the real client end to end —
so you can confirm what does and doesn't leave your machine, with a network
inspector if you like.

**A note on verifiable builds (honest status).** The binaries tree proves that the
release artifacts you downloaded are byte-for-byte the ones the pipeline logged and
signed — it is a tamper-evident **binding**, not yet a *reproducible* build. Today
binary integrity at install time still rests on platform code-signing (Apple
Developer ID + notarization, Azure Trusted Signing). Full bit-for-bit
reproducibility, cosign/SLSA provenance, and an in-app "verify this build" button
are designed but **not shipped** — tracked in
[#484](https://github.com/actuallydan/pollis/issues/484) and
[docs/verifiable-builds-design.md](docs/verifiable-builds-design.md).

## Releases

Builds for macOS, Windows, and Linux are published automatically on every version
tag via the Tauri release workflow. Tauri's bundler produces the platform
installers and the `update-{{bundle_type}}.json` manifests that the in-app updater
reads at runtime from `cdn.pollis.com`, so the marketing site at
[pollis.com](https://pollis.com) always shows the current download links.
Auto-update trust is rooted in the OS code signature on each installer — Apple
Developer ID + notarization on macOS, Azure Trusted Signing on Windows — and each
released artifact is additionally logged to the binaries transparency tree (see
[Verify it yourself](#verify-it-yourself)).

![Pollis UI](readme/new_app.png)

## Contributing

Build, run, and test instructions — plus the conventions changes are expected to
follow — are in **[CONTRIBUTING.md](CONTRIBUTING.md)**. The architecture overview
is in [CLAUDE.md](CLAUDE.md) and [ARCHITECTURE.md](ARCHITECTURE.md); subsystem docs
are in [`.codesight/wiki/`](.codesight/wiki/index.md).

## What this repo produces

This is a monorepo. Despite the desktop app being the headline, it ships a number
of distinct, independently-deployable artifacts:

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

The reusable backend (`pollis-core`) is the shared spine: the desktop host, the
mobile bindings, and the CLIs all build on it.

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

- **Broader platform availability** — currently open pre-alpha; working toward a
  stable public release
- **Post-quantum hybrid key exchange** (X25519 + ML-KEM-768) and an optional
  IP-hiding relay overlay — designed, deferred; see the security docs above
