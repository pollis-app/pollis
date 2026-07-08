# Deployments — what we build, where each directory goes, and how it ships

The single map of **every product and service this repo produces**, which
directories/crates build into each, the GitHub Actions workflow that builds or
deploys it, and where it runs. New here? Start with this file to understand what
the codebase actually *ships*. Keep it updated when a build/deploy pipeline
changes.

There are **4 shipped executables/sites**, **3 running backend services**, and
**2 managed data layers**, plus **4 CI-only gates**.

---

## Directory → output map

| Directory | What it is | Ships as part of |
|---|---|---|
| `src-tauri/` | Tauri desktop host (the shipping shell): `tauri::Builder`, `invoke_handler`, window/tray/lifecycle | **Desktop app** |
| `frontend/` | React/TS renderer (Vite) — the desktop UI, and the browser dev target | **Desktop app** (+ `pnpm dev:frontend`) |
| `pollis-core/` | Reusable Rust core (auth, MLS, groups/channels/DMs, messages, DB, media) — no shell dependency | **Desktop app, CLI, Mobile** (shared) |
| `pollis-tui/` | Headless terminal client (`pollis` binary, ratatui) on `pollis-core` — no Tauri | **CLI** |
| `pollis-delivery/` | The Delivery Service (DS): axum server, sole writer to Turso, MLS commit serialization | **DS** (backend service) |
| `pollis-capture-{linux,macos,proto}/` | Screen-capture helpers for the desktop media pipeline | **Desktop app** (media stack) |
| `mobile/` | React Native / Expo app, consumes `pollis-core` via uniffi bindings | **Mobile app** (in development — epics #342/#339) |
| `verifiable-log/` | Core Merkle tree / STH / inclusion-proof crate | **Transparency log** + **pollis-verify** |
| `verifiable-log-builder/` | Builds the commit-log / account-key / **binaries** tenant trees | **Transparency log** (build side) |
| `verifiable-log-serve/` | Static read API + the `pollis-verify` auditor CLI | **Transparency log** (serve) + **pollis-verify** |
| `website/` | Static marketing site (HTML/CSS/JS) | **Website** |
| `livekit/` | LiveKit + nginx compose/ingress config | **LiveKit media stack** (backend service) |
| `aur/`, `assets/`, `readme/` | AUR packaging, icons/assets, README media | packaging/support (not standalone outputs) |
| `e2e/` | WebDriver end-to-end tests (real binary + WebKitGTW) | CI / local only — not shipped |
| `supply-chain/` | cargo-vet audit ledger (`audits.toml`, `config.toml`, `imports.lock`) | CI gate input (not shipped) |
| `scripts/`, `docs/`, `.github/` | build/deploy scripts, docs, CI workflows | tooling |

---

## Products (user-facing — built + distributed)

### 1. Desktop app (Tauri)
- **From:** `src-tauri/` + `frontend/` + `pollis-core/` + `pollis-capture-*`
- **Pipeline:** `.github/workflows/desktop-release.yml` — triggered by a `git tag v*` push (the whole ceremony: version stamp, signed macOS DMG/ZIP, Windows via Azure Trusted Signing, Linux .deb/.rpm/.AppImage + AUR, R2 upload, GitHub release, `latest.json` + Tauri updater manifests, and `apply-migrations` to prod Turso).
- **Ships to:** `cdn.pollis.com/releases/<version>/…` + auto-update manifests. Install: the download cards / `curl … cdn.pollis.com/releases/install.sh | bash` on the site.

### 2. CLI / terminal client (`pollis`)
- **From:** `pollis-tui/` + `pollis-core/` (headless, no Tauri)
- **Pipeline:** `.github/workflows/cli-release.yml` — `workflow_dispatch` (version input). Builds per-platform binaries (Linux glibc-dynamic, macOS aarch64, Windows), bakes prod creds via `option_env!` (read-only Turso token only).
- **Ships to:** `cdn.pollis.com/releases/cli/<version>/…` + `cli-latest.json` + `cli-install.sh`. Install: `curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | bash`.

### 3. Marketing website
- **From:** `website/`
- **Pipeline:** `.github/workflows/website-deploy.yml` — `workflow_dispatch` (deploy-button pattern; `main` stays always-deployable).
- **Ships to:** Cloudflare Pages → **pollis.com** / **www.pollis.com**.

### 4. pollis-verify (auditor CLI)
- **From:** `verifiable-log-serve/` (+ `verifiable-log*`)
- **Pipeline:** `.github/workflows/verifier-release.yml` — builds the standalone verifier binaries + `SHA256SUMS`, with the **pinned Ed25519 public key** in the release body. Lets any analyst independently verify the transparency log.
- **Ships to:** GitHub release assets. Subcommands: `remote` / `group` / `account` / `release` (verify the whole log, a conversation's commit chain, a user's key history, or a released version's binaries).

### (in development) Mobile app
- **From:** `mobile/` + `pollis-core/` (uniffi). Epics #342 (RN/Expo) + #339/#340/#341 (App Store / Play Store distribution). Not yet a released output.

---

## Backend services (we run/host)

### Delivery Service (DS) — the API server
- **From:** `pollis-delivery/`
- **Code deploys:** `.github/workflows/delivery-deploy-prod.yml` and `delivery-deploy-dev.yml` — `workflow_dispatch` (`-f ref=main`). Builds the image on GitHub runners (off the VPS to avoid LiveKit CPU contention) with the git SHA baked in (`--build-arg GIT_SHA`), pushes to **GHCR** (`:prod` / `:dev`), pokes a token-gated **Watchtower** endpoint on the VPS which recreates the container **preserving its env**, then **verifies the new build is actually live** by polling `/version` until it reports the built SHA (fails the deploy otherwise). No secrets ever touch GitHub.
  - ⚠️ The Watchtower poke is **unfiltered** on purpose. Its HTTP-API `?image=` filter matches nothing here and silently no-op'd every deploy for weeks (found 2026-07-08 — both containers were 11 days stale) until the `/version` check surfaced it. Only `delivery`/`delivery-dev` carry the `com.centurylinklabs.watchtower.enable` label and track distinct tags, so an unfiltered poke updates just the one env's container.
- **Env / secret changes (rare):** run `deploy/delivery.sh dev|prod` **on the VPS** — it `doppler run`s the full env from Doppler (`dev_personal` / `prd_prod`) and recreates the container. This is the ONE place DS runtime env is injected; Watchtower preserves it across subsequent code deploys. (Doppler is the complete source of truth: `TURSO_*`, `LOG_DB_*`, `RESEND_API_KEY`, `LIVEKIT_*`, `R2_*` incl. `R2_BUCKET`, `TURSO_PLATFORM_TOKEN/ORG/DB`, `PORT`, `POLLIS_DS_REQUIRE_AUTH`, dev `DEV_OTP`.)
- **Runs at:** VPS (`downpage`-class) → **api.pollis.com** (prod) / **api-dev.pollis.com** (dev). Health: `/health`; build SHA: `/version`.
- **Role:** sole writer to Turso; clients hold read-only tokens and write only via the DS (structural commit-log integrity, #419/#420). Also the authorized-secrets broker (`/v1/livekit/*`, `/v1/r2/presign`, `/v1/turso/token` — #393).

### LiveKit + nginx — media SFU (voice / video / screenshare)
- **From:** `livekit/` (compose + nginx ingress; runs **upstream** LiveKit images, not our build)
- **Pipeline:** `.github/workflows/livekit-deploy.yml` — `workflow_dispatch` (env choice prod/dev). SSH + compose on the VPS.
- **Runs at:** VPS. Frames are E2EE (the SFU forwards ciphertext).

### Transparency log — Key Transparency read API
- **From:** `verifiable-log*` (built + signed in CI, served as **static files** — no server on the trust path)
- **Pipeline:** `.github/workflows/transparency-publish.yml` — **daily cron** (06:47 UTC). Rebuilds the commit-log + account-key trees from the DB, signs STHs in CI, syncs to Cloudflare R2, self-audits + tripwire. (The `binaries` tenant tree, #453, is built into the crates but not yet wired into this publish job — that's #453 Phase 2.)
- **Runs at:** static files on Cloudflare R2 → **verify.pollis.com**.

---

## Managed data / storage

- **Turso** (libSQL) — two databases: the **main** DB (users, groups, membership, public keys, encrypted envelopes) and the **commit-log** DB (`mls_commit_log` / `mls_group_info` / `mls_welcome`). Schema is applied by the `apply-migrations` job in `desktop-release.yml` (nobody applies to prod by hand); numbered migrations in `pollis-core/src/db/migrations/`, additive-only.
- **Cloudflare R2** — object storage behind **cdn.pollis.com**: desktop + CLI releases, install scripts, and the transparency-log static tree.

---

## CI gates (build/verify only — never deployed)

| Workflow | Gates |
|---|---|
| `mls-tests.yml` | DS serializer, MLS crypto/state-machine unit tests, multi-client integration flows harness + marathon soak (protects the bulletproof-membership invariants). Runs on any Rust/workspace change. |
| `kani.yml` | Kani bounded-model-checking proofs on `pollis-core` + `pollis-delivery` pure fns (watermark no-skip, recovery gate, canonicalization, gap/head arithmetic). Path-filtered to `pollis-core/**`. |
| `supply-chain.yml` | cargo-deny (advisories/licenses/bans/sources) + cargo-vet (dependency review provenance). Runs on every PR. |
| `verifiable-log-tests.yml` | `cargo test` on the three `verifiable-log*` crates (transparency infra + `pollis-verify`). Pure Rust, no system deps. |

> Gap (as of this writing): the **frontend** (`frontend/`) has no CI gate — no `tsc`/`vite build`/lint check. A change there can merge without a typecheck. Adding a frontend CI job would close it (the renderer builds headless: `pnpm --filter frontend build`).

---

## Quick "what do I deploy when I change X?"

| You changed… | Redeploy / re-release |
|---|---|
| `website/` | `website-deploy.yml` |
| `pollis-delivery/` (or `pollis-core` DS paths) | `delivery-deploy-dev.yml` → verify → `delivery-deploy-prod.yml` |
| `livekit/` config | `livekit-deploy.yml` |
| `pollis-core` / `src-tauri` / `frontend` (desktop-facing, user-visible) | tag a new `v*` → `desktop-release.yml` (also releases the DB migrations) |
| `pollis-tui` / `pollis-core` (CLI-facing) | `cli-release.yml` |
| a DB migration in `pollis-core/src/db/migrations/` | ships with the next `desktop-release.yml` (`apply-migrations`) |
| `verifiable-log*` (published-tree behavior) | `transparency-publish.yml` (also runs daily) |
| `verifiable-log-serve` (`pollis-verify` CLI) | `verifier-release.yml` |
