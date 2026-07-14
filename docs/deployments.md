# Deployments — what we build, where each directory goes, and how it ships

The single map of **every product and service this repo produces**, which
directories/crates build into each, the GitHub Actions workflow that builds or
deploys it, and where it runs. New here? Start with this file to understand what
the codebase actually *ships*. Keep it updated when a build/deploy pipeline
changes.

There are **4 shipped executables/sites**, **3 running backend services**, and
**2 managed data layers**, plus **8 CI-only gates**.

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
- **Ships to:** `cdn.pollis.com/releases/cli/<version>/…` + the `https://cdn.pollis.com/releases/cli/latest.json` manifest (which `cli-install.sh` fetches) + `cli-install.sh`. Install: `curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | bash`.

### 3. Marketing website
- **From:** `website/`
- **Pipeline:** `.github/workflows/website-deploy.yml` — `workflow_dispatch` (deploy-button pattern; `main` stays always-deployable).
- **Ships to:** Cloudflare Pages → **pollis.com** / **www.pollis.com**.

### 4. pollis-verify (auditor CLI)
- **From:** `verifiable-log-serve/` (+ `verifiable-log*`)
- **Pipeline:** `.github/workflows/verifier-release.yml` — triggered by a `pollis-verify-v*` tag push (a `workflow_dispatch` builds run artifacts only, no Release). Builds the standalone verifier binaries, each with a per-asset `.sha256` checksum file, with the **pinned Ed25519 public key** in the release body. Lets any analyst independently verify the transparency log.
- **Ships to:** GitHub release assets. Subcommands: `remote` / `group` / `account` / `release` (verify the whole log, a conversation's commit chain, a user's key history, or a released version's binaries).

### (in development) Mobile app
- **From:** `mobile/` + `pollis-core/` (uniffi). Epics #342 (RN/Expo) + #339/#340/#341 (App Store / Play Store distribution). Not yet a released output.

---

## Backend services (we run/host)

### Delivery Service (DS) — the API server
- **From:** `pollis-delivery/`
- **Runs on:** [Cloudflare Containers](https://developers.cloudflare.com/containers/) — the existing `pollis-delivery/Dockerfile` runs behind a Worker front-door + Durable Object (`worker/index.ts`, `PollisDelivery` class). The DO gives exactly **1 serialized instance** (sole-writer invariant, #419/#420); the Worker forwards every request to the container on `:8788` (no per-route allowlist). `sleepAfter: 10m` = scale-to-zero pre-launch (drop it before real users — see #515).
- **Code deploys:** `.github/workflows/delivery-deploy-{dev,prod}.yml` — both **`workflow_dispatch`-only** (fire at will; a batch of merges doesn't churn CI), with an optional `ref` input. Each run: **apply pending DB migrations first** (migrate-then-ship — `db-apply.sh` against the main DB then the commit-log DB, so the DS never runs code ahead of its schema; the deploy fails if a migration fails), sync secrets Doppler → Wrangler Secrets Store, stamp the git SHA into the container build arg, `wrangler deploy --containers-rollout immediate`, then **verify the new build is live** by polling `/version` for the built SHA (the #509 tripwire; on a genuine stall it dumps `wrangler containers list`/`instances` for diagnosis). Dev and prod are **separate wrangler configs** (`wrangler.dev.jsonc` / `wrangler.prod.jsonc`), so a dev deploy structurally cannot touch prod.
- **Secrets:** Doppler (`dev_personal` / `prd_prod`) stays the single source of truth. The deploy workflow (`.github/scripts/sync-ds-secrets.sh`) upserts each key into the account's Secrets Store namespaced `DS_DEV_` / `DS_PROD_`; the Worker resolves them at container start and injects them as OS env vars. Keys: `TURSO_*`, `LOG_DB_*`, `RESEND_API_KEY`, `LIVEKIT_*`, `R2_*` incl. `R2_BUCKET`, `TURSO_PLATFORM_TOKEN/ORG/DB`, dev `DEV_OTP`. `PORT`/`POLLIS_DS_REQUIRE_AUTH` are non-secret wrangler `vars`.
- **Rotating a DS secret** (Turso token, LiveKit key, etc. — no code change): update Doppler, then run the deploy workflow with **`force_restart: true`**. CF Containers do **not** restart on a secret-only change (the running instance keeps the old value until a new *image digest* deploys), so `force_restart` bumps `image_vars.BUILD_NONCE` to a unique value → new digest → the container rolls and re-reads the Secrets Store on restart (`worker/index.ts` `resolveSecretEnv` runs on every start). A normal code deploy rolls via the `GIT_SHA` change and needs no flag. (The `BUILD_NONCE` arg in the Dockerfile is a runtime no-op; self-hosters running the image directly ignore it.)
- **Container lifecycle (why deploys are reliable):** the DS traps **SIGTERM** and exits within ~5s (`pollis-delivery/src/main.rs` `shutdown_signal`, with a hard-exit backstop). This matters because with `max_instances: 1` CF does **stop-first / drain-then-replace** — it SIGTERMs the old instance (grace up to 15 min) before starting the new one. The DS runs as **PID 1**, which ignores unhandled signals, so without the handler the old instance would squat the whole grace window and the swap/verify would stall (the original "container won't swap" bug). Stop-first is the correct strategy for a single-writer service — it never runs two writers — and a momentary overlap would be harmless anyway: the commit-log **CAS insert** (`commit.rs`: `INSERT … WHERE epoch = MAX(epoch)+1 … ON CONFLICT DO NOTHING` in an `IMMEDIATE` txn, backed by `UNIQUE(conversation_id, epoch)`) rejects any stale/out-of-order write. The ~1–3s deploy blip is retryable 503s; clients already retry.
- **Required CI config** (per GH environment `delivery-dev` / `delivery-prod`): secrets `CLOUDFLARE_API_TOKEN` (scoped: Workers Scripts\:Edit + Containers + Secrets Store write — **not** the broad R2/DNS token), `CLOUDFLARE_ACCOUNT_ID`, `DOPPLER_TOKEN` (service token for that env's config); var `SECRETS_STORE_ID`.
- **Runs at:** **api.pollis.com** (prod) / **api-dev.pollis.com** (dev) via Worker custom domains (route change on the CF zone; rollback = flip DNS back to the VPS). Health: `/health`; build SHA: `/version`.
  - **History (#515):** replaced the old GHCR-build + VPS-Watchtower path (which silently no-op'd deploys for 11 days). Cut over 2026-07-08/09; the VPS `delivery`/`delivery-dev`/`watchtower` containers are **stopped-but-present** as rollback (restart + flip DNS back to `31.97.140.76`). Final teardown (delete containers, remove nginx api/api-dev vhosts, GHCR images) after the soak.
- **Role:** sole writer to Turso; clients hold read-only tokens and write only via the DS (structural commit-log integrity, #419/#420). Also the authorized-secrets broker (`/v1/livekit/*`, `/v1/r2/presign`, `/v1/turso/token` — #393).

### LiveKit + nginx — media SFU (voice / video / screenshare)
- **From:** `livekit/` (compose + nginx ingress; runs **upstream** LiveKit images, not our build)
- **Pipeline:** `.github/workflows/livekit-deploy.yml` — `workflow_dispatch` (env choice prod/dev). SSH + compose on the VPS.
- **Runs at:** VPS. Frames are E2EE (the SFU forwards ciphertext).

### Transparency log — Key Transparency read API
- **From:** `verifiable-log*` (built + signed in CI, served as **static files** — no server on the trust path)
- **Pipeline:** `.github/workflows/transparency-publish.yml` — **daily cron** (06:47 UTC). Rebuilds the commit-log + account-key trees from the DB and the **binaries** tenant tree (#453) from the accumulating BinaryRecord JSON on R2 that `desktop-release.yml`'s `attest-and-log` job appends to at release time; signs STHs in CI, syncs to R2, self-audits + tripwire. The binaries tree is live at `https://verify.pollis.com/v1/binaries/sth/latest.json`, with per-tag reports under `verify/release/<tag>` (verifiable via `pollis-verify release`).
- **Runs at:** static files on Cloudflare R2 → **verify.pollis.com**.

---

## Managed data / storage

- **Turso** (libSQL) — two databases: the **main** DB (users, groups, membership, public keys, encrypted envelopes) and the **commit-log** DB (`mls_commit_log` / `mls_group_info` / `mls_welcome`). Schema is applied **migrate-then-ship** by whichever deploy touches prod first: the `apply-migrations` job in `desktop-release.yml` (client releases) **and** the `delivery-deploy-{dev,prod}.yml` deploys (DS releases) both run `db-apply.sh` before shipping. It's idempotent (tracks `schema_migrations`), so overlap is harmless, and additive-only migrations make early application safe for the still-running old code. Nobody applies to prod by hand. Numbered migrations in `pollis-core/src/db/migrations/`; dev also auto-applies on merge via `db-migrate-dev.yml`.
- **Cloudflare R2** — object storage behind **cdn.pollis.com**: desktop + CLI releases, install scripts, and the transparency-log static tree.

---

## CI gates (build/verify only — never deployed)

| Workflow | Gates |
|---|---|
| `mls-tests.yml` | DS serializer, MLS crypto/state-machine unit tests, multi-client integration flows harness + marathon soak (protects the bulletproof-membership invariants). Runs on any Rust/workspace change. |
| `kani.yml` | Kani bounded-model-checking proofs on `pollis-core` + `pollis-delivery` pure fns (watermark no-skip, recovery gate, canonicalization, gap/head arithmetic). Path-filtered to `pollis-core/**`. |
| `supply-chain.yml` | cargo-deny (advisories/licenses/bans/sources) + cargo-vet (dependency review provenance). Runs on every PR. |
| `verifiable-log-tests.yml` | `cargo test` on the three `verifiable-log*` crates (transparency infra + `pollis-verify`). Pure Rust, no system deps. |
| `tla.yml` | TLC exhaustively model-checks both TLA+ specs — Spec A CommitLog (invariants I1+I2) + Spec B Delivery (I3+I4) — plus a "teeth" check that each broken variant still produces a counterexample. JVM-only, no Rust build. Path-filtered to `specs/tla/**`. |
| `e2e-smoke.yml` | WebDriver smoke of the **real Tauri binary**: does the app launch and show the login screen (`e2e/smoke.js`, no DS / shared-Turso dependency). `workflow_dispatch`-only — a full cargo build + virtual WebKitGTK window is too heavy for every push; run it before a release or after touching auth/bootstrap. |
| `mobile-core-check.yml` | Cross-compiles `pollis-core` for Android + iOS (aarch64, `--no-default-features`, matching the ubrn build) so mobile `#[cfg]` gate rot becomes red CI instead of a latent defect on the next mobile build. Path-filtered to `pollis-core/**`. |
| `frontend-check.yml` | Renderer typecheck on every PR to `main`: filtered pnpm install of `frontend/` only, then plain `tsc` (noEmit — no vite build, no artifacts). A frontend change can no longer merge without a typecheck. |

### Dispatch-only release-verification tooling (`workflow_dispatch`, take a released tag)

| Workflow | Does |
|---|---|
| `attest-release.yml` | Backfills the binary-transparency attest step for an **already-published tag** — no rebuild (~2 min vs a ~40 min release). Deliberately duplicates `desktop-release.yml`'s built-in `attest-and-log` job: same `scripts/attest-binaries.sh`, the tag's commit timestamp, the published release assets. Idempotent — a tag already in the accumulator is a no-op. |
| `rebuild-verify.yml` | The **third-party reproducer** (#484): rebuilds a released tag's Linux AppImage from public source with **no Pollis secrets** — runnable from a fork — and asserts the payload hash against the transparency log, trusting only the pinned Ed25519 log key. Always proves **log inclusion**; bit-for-bit **reproduction** additionally needs the published build recipe supplied as non-secret repo `vars` (some `option_env!` values are still secret — see `docs/reproducible-builds-residuals.md`). |

---

## Quick "what do I deploy when I change X?"

| You changed… | Redeploy / re-release |
|---|---|
| `website/` | `website-deploy.yml` |
| `pollis-delivery/` (or `pollis-core` DS paths) | `delivery-deploy-dev.yml` → verify → `delivery-deploy-prod.yml` |
| `livekit/` config | `livekit-deploy.yml` |
| `pollis-core` / `src-tauri` / `frontend` (desktop-facing, user-visible) | tag a new `v*` → `desktop-release.yml` (also releases the DB migrations) |
| `pollis-tui` / `pollis-core` (CLI-facing) | `cli-release.yml` |
| a DB migration in `pollis-core/src/db/migrations/` | applied by whichever runs first: a DS deploy (`delivery-deploy-{dev,prod}.yml`) or `desktop-release.yml` (`apply-migrations`) — both migrate-then-ship, idempotent; dev also auto-applies on merge via `db-migrate-dev.yml` |
| `verifiable-log*` (published-tree behavior) | `transparency-publish.yml` (also runs daily) |
| `verifiable-log-serve` (`pollis-verify` CLI) | `verifier-release.yml` |
