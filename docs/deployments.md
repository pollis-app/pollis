# Deployments — what we build, where each directory goes, and how it ships

The single map of **every product and service this repo produces**, which
directories/crates build into each, the GitHub Actions workflow that builds or
deploys it, and where it runs. New here? Start with this file to understand what
the codebase actually *ships*. Keep it updated when a build/deploy pipeline
changes.

There are **4 shipped executables/sites**, **4 running backend services**, and
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
| `pollis-relay/` | Closed-overlay relay: QUIC-in / allowlisted TCP-dial-out byte pipe, offline device-cert auth, no Turso creds (`pollis-relay` binary) | **Relay pool** (backend service) |
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
- **R2 retention:** the release job keeps only the **latest 3** `releases/v*/` version directories on R2, pruning older ones **at release time** (count-based, not an R2 lifecycle/age rule — an age rule would delete the *current* release if releases went quiet). It never prunes the version it just published, the top-level `install.sh`/`latest.json`/`update-*.json`, the `releases/latest/` alias, or `cli/`. **GitHub Releases remain the permanent archive** of every version's installers, and the transparency log retains every binary hash — so R2 only needs a small rolling window (the updater/install.sh/AUR/website reference only the latest, plus a couple for rollback headroom). The no-JS website download links point at the stable `releases/latest/pollis-latest-*` alias (overwritten each release, never pruned).

### 2. CLI / terminal client (`pollis`)
- **From:** `pollis-tui/` + `pollis-core/` (headless, no Tauri)
- **Pipeline:** `.github/workflows/cli-release.yml` — `workflow_dispatch` (version input). Builds per-platform binaries (Linux glibc-dynamic, macOS aarch64, Windows), bakes prod creds via `option_env!` (read-only Turso token only).
- **Ships to:** `cdn.pollis.com/releases/cli/<version>/…` + the `https://cdn.pollis.com/releases/cli/latest.json` manifest (which `cli-install.sh` fetches) + `cli-install.sh`. Install: `curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | bash`.

### 3. Marketing website
- **From:** `website/`
- **Pipeline:** `.github/workflows/website-deploy.yml` — `workflow_dispatch` (deploy-button pattern; `main` stays always-deployable).
- **Ships to:** Cloudflare Pages → **pollis.com** / **www.pollis.com**.
- **`/learn` section (Epic #589):** `website/learn.html` + `website/learn.js`, media under `website/learn/media/*` (mp4/webm/poster/vtt). The video artifacts are **committed to git** and served straight from the Pages build (no build step). Animation sources + narration scripts live in `learn/manim/`; regenerate with `learn/manim/render.sh <Scene> <slug>` (see `learn/README.md`). Long-term, rendered media may move to R2 (one-line base-URL swap) to cap clone size.

### 4. pollis-verify (auditor CLI)
- **From:** `verifiable-log-serve/` (+ `verifiable-log*`)
- **Pipeline:** `.github/workflows/verifier-release.yml` — triggered by a `pollis-verify-v*` tag push (a `workflow_dispatch` builds run artifacts only, no Release). Builds the standalone verifier binaries, each with a per-asset `.sha256` checksum file, with the **pinned log public key** in the release body (ML-DSA-44 since #668, rotated to fresh material in #732). Lets any analyst independently verify the transparency log.
- **Ships to:** GitHub release assets. Subcommands: `remote` / `group` / `account` / `release` (verify the whole log, a conversation's commit chain, a user's key history, or a released version's binaries).
- **Versioning (#670):** this is the one output whose version lives in its own manifest. `verifiable-log-serve/Cargo.toml` carries a concrete `version` — deliberately **not** `version.workspace = true`, which would inherit the frozen `1.1.0` placeholder — so `--version` names the release tag an auditor downloaded. **Bump it in the PR that cuts the tag**; the workflow asserts `pollis-verify-v<version>` matches the manifest and fails the release if they drift. `--version` also prints the build's commit (`POLLIS_VERIFY_GIT_SHA`, set by `verifier-release.yml` and `rebuild-verify.yml`), or `source build` for a plain `cargo build` — so a local build is never mistaken for a release.

### (in development) Mobile app
- **From:** `mobile/` + `pollis-core/` (uniffi). Epics #342 (RN/Expo) + #339/#340/#341 (App Store / Play Store distribution). Not yet a released output.
- **CI build (#706 / PL-18):** the `android-build` job in `mobile-core-check.yml` is the **only** thing that assembles a real installable artifact today. On a stock `ubuntu-latest` runner it runs the full chain — uniffi binding generation + a 3-ABI (`arm64-v8a` / `armeabi-v7a` / `x86_64`) `cargo-ndk` cross-compile of `pollis-core`, `expo prebuild --platform android`, then `gradlew :app:assembleRelease` — and uploads the APK as the **`pollis-android-apk`** workflow artifact. It uses `expo prebuild` + Gradle **directly, not `eas build`** (there is no EAS account). The APK is signed with the Expo template's throwaway **debug keystore** — installable for internal testing, **not** distribution-signed. No signing secret is read; the job needs none.
- **`eas.json` profiles** (`mobile/eas.json`) — `development` / `preview` / `production`, authored for when an EAS account exists. `cli.appVersionSource: "local"` (version lives in `app.json`; no EAS server holds it). **`runtimeVersion.policy: "fingerprint"` on every profile** is deliberate: `pollis-core` is a native Rust library reached over uniffi, so a JS-only OTA update is unsafe whenever the core changes. `fingerprint` derives the runtime version from a hash of the native layer, so any native change yields a new runtime version and an incompatible OTA can never reach an old binary — a `sdkVersion`/`appVersion` policy would silently allow exactly that. No `credentials` block and no `extra.eas.projectId` — both need accounts this repo does not have.
- **Still manual / blocked (PL-18 remainder):** iOS build + Apple distribution signing (needs an Apple Developer account #723 + a current-gen Mac with Xcode 26); Android Play **upload signing** (needs the Play console); `expo.extra.eas.projectId` (needs an Expo/EAS account, also gates push #344/PL-19). The `mobile/CLAUDE.md` "Environment" notes cover running the same build on a workstation.

---

## Backend services (we run/host)

### Delivery Service (DS) — the API server
- **From:** `pollis-delivery/`
- **Runs on:** [Cloudflare Containers](https://developers.cloudflare.com/containers/) — the existing `pollis-delivery/Dockerfile` runs behind a Worker front-door + Durable Object (`worker/index.ts`, `PollisDelivery` class). The DO gives exactly **1 serialized instance** (sole-writer invariant, #419/#420); the Worker forwards every request to the container on `:8788` (no per-route allowlist). `sleepAfter: 10m` = scale-to-zero pre-launch — a **cost decision, not a pending code fix** (see "Cold start / always-on" below).
- **Code deploys:** `.github/workflows/delivery-deploy-{dev,prod}.yml` — both **`workflow_dispatch`-only** (fire at will; a batch of merges doesn't churn CI), with an optional `ref` input. Each run: **apply pending DB migrations first** (migrate-then-ship — `db-apply.sh` against the main DB then the commit-log DB, so the DS never runs code ahead of its schema; the deploy fails if a migration fails), sync secrets Doppler → Wrangler Secrets Store, **build the container image ourselves under a persistent layer cache and push it, then deploy that pre-built image** (see the next bullet), then **verify the new build is live** by polling `/version` for the built SHA (the #509 tripwire; on a genuine stall it dumps `wrangler containers list`/`instances` for diagnosis). Dev and prod are **separate wrangler configs** (`wrangler.dev.jsonc` / `wrangler.prod.jsonc`), so a dev deploy structurally cannot touch prod.
- **Image build & caching (#696) — we build the image, wrangler does NOT:** the deploy `docker buildx build`s the image (`docker/build-push-action`, `--platform linux/amd64`, `--provenance=false`) under a **persistent BuildKit layer cache** (`--cache-from type=gha --cache-to type=gha,mode=max`, scope `pollis-delivery`, shared by dev+prod since both deploy from `main`), `wrangler containers push`es it to the Cloudflare managed registry (`registry.cloudflare.com/<ACCOUNT_ID>/pollis-delivery-{dev,prod}:<SHA>`), then rewrites the wrangler config `image` to that registry reference so **`wrangler deploy` performs no `docker build` of its own**. The `GIT_SHA` (for the `/version` gate) and `BUILD_NONCE` are passed as buildx `--build-arg`s, not `image_vars`. Paired with a **cargo-chef dependency layer** in `pollis-delivery/Dockerfile`: the `cook` stage compiles only third-party deps and is a real image layer the `type=gha` cache restores, so an ordinary code change recompiles only first-party crates (`pollis-delivery` + workspace members like `pollis-core`) — the deploy-time win the ticket was about.
  - **Why the old path could never cache — do not revert to `image: ./Dockerfile`.** `wrangler deploy` built the image **locally with the local Docker daemon** (Cloudflare has no remote/server-side build: [image-management docs](https://developers.cloudflare.com/containers/platform-details/image-management/)) by shelling out to a plain `docker build --load … -f - <ctx>` — **no `--cache-from`, no `--cache-to`, no buildx** (wrangler `packages/containers-shared/src/build.ts`, verified against 4.118.0). Cloudflare's "cached image layers" is **push-time blob dedup** ("only push layers that have changed"), i.e. the registry skips blobs it already holds — **not** build-time caching; `docker build` still runs in full. And both delivery deploy workflows run on `ubuntu-latest` with **no `docker/setup-buildx-action` and no cache restore**, so BuildKit's cache was **empty on every run** (matches cloudflare/workers-sdk #14375, `Reclaimable: 0B` after a wrangler deploy). Adding cargo-chef to the Dockerfile *alone* was therefore worthless (marginally harmful); it only pays off once CI owns the build under a warm cache, so the two ship together. **First-two-deploys check:** on the second deploy after this merges, the `Build image (buildx, cached)` step should show `CACHED` on the `cargo chef cook` stage and finish materially faster than the first (cold) run.
- **Secrets:** Doppler (`dev_personal` / `prd_prod`) stays the single source of truth. The deploy workflow (`.github/scripts/sync-ds-secrets.sh`) upserts each key into the account's Secrets Store namespaced `DS_DEV_` / `DS_PROD_`; the Worker resolves them at container start and injects them as OS env vars. Keys: `TURSO_*`, `LOG_DB_*`, `RESEND_API_KEY`, `LIVEKIT_*`, `R2_*` incl. `R2_BUCKET`, `TURSO_PLATFORM_TOKEN/ORG/DB`, dev `DEV_OTP`, and `POLLIS_DS_METRICS_TOKEN` (see below). `PORT`/`POLLIS_DS_REQUIRE_AUTH`/`POLLIS_DS_GC_SWEEP_SECS`/`POLLIS_DS_WATERMARK_STALE_MONTHS`/`POLLIS_DS_SECURITY_EVENT_RETENTION_DAYS` (default 90)/`POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS` (default 180) are non-secret wrangler `vars` (the staleness window is set to **12**; see `docs/metadata-retention-policy.md` §1).
- **`GET /v1/config` (#760) — the one-request answer to "did the config actually arrive?"** Same
  operator bearer token as the metrics route, and the same fail-closed shape. Returns the DS's
  *effective* config (the staleness modifier the GC really binds, the sweep interval, whether auth is
  enforced, and whether a metrics token is set — **never** a secret's value). It exists because a DS
  config value crosses five boundaries (Doppler → `sync-ds-secrets.sh` `KEYS` → wrangler binding/`var`
  → the Worker's `SECRET_KEYS`/`envVars` → the process) and #720's two values were silently dropped at
  the fourth, through a fully green deploy. **A 404 here means the config never arrived; a 401 means it
  did and auth rejected** — the prod deploy now asserts that distinction.
- **`POLLIS_DS_METRICS_TOKEN` (#720) — set it to enable retention metrics.** It gates `GET /v1/retention/metrics` (identity-free `message_envelope` growth snapshot). The endpoint is **default-closed**: **unset → the route 404s and no metrics are served** (fail-closed, because the snapshot is a pollable aggregate activity meter, not health-check-class data); set → the owner's scraper must send `Authorization: Bearer <token>` (`curl -H "Authorization: Bearer $TOKEN" https://api.pollis.com/v1/retention/metrics`). It is a **secret** (an access credential), so it lives in Doppler alongside the other DS secrets, not the wrangler `vars`. Alert *routing* (thresholds/paging) is still the owner's console — the DS only emits the numbers.
- **Rotating a DS secret** (Turso token, LiveKit key, etc. — no code change): update Doppler, then run the deploy workflow with **`force_restart: true`**. CF Containers do **not** restart on a secret-only change (the running instance keeps the old value until a new *image digest* deploys), so `force_restart` builds with a unique `--build-arg BUILD_NONCE` **and** pushes/deploys under a unique image tag (`<SHA>-r<run_id>`) → new digest + new reference → the container rolls and re-reads the Secrets Store on restart (`worker/index.ts` `resolveSecretEnv` runs on every start). A normal code deploy rolls via the new `<SHA>` tag and needs no flag. (The `BUILD_NONCE` arg in the Dockerfile is a runtime no-op; self-hosters running the image directly ignore it.)
- **Container lifecycle (why deploys are reliable):** the DS traps **SIGTERM** and exits within ~5s (`pollis-delivery/src/main.rs` `shutdown_signal`, with a hard-exit backstop). This matters because with `max_instances: 1` CF does **stop-first / drain-then-replace** — it SIGTERMs the old instance (grace up to 15 min) before starting the new one. The DS runs as **PID 1**, which ignores unhandled signals, so without the handler the old instance would squat the whole grace window and the swap/verify would stall (the original "container won't swap" bug). Stop-first is the correct strategy for a single-writer service — it never runs two writers — and a momentary overlap would be harmless anyway: the commit-log **CAS insert** (`commit.rs`: `INSERT … WHERE epoch = MAX(epoch)+1 … ON CONFLICT DO NOTHING` in an `IMMEDIATE` txn, backed by `UNIQUE(conversation_id, epoch)`) rejects any stale/out-of-order write. The ~1–3s deploy blip is retryable 503s; clients already retry.
- **Required CI config** (per GH environment `delivery-dev` / `delivery-prod`): secrets `CLOUDFLARE_API_TOKEN` (scoped: Workers Scripts\:Edit + Containers + Secrets Store write — **not** the broad R2/DNS token) and `DOPPLER_TOKEN` (service token for that env's config); vars `CLOUDFLARE_ACCOUNT_ID` and `SECRETS_STORE_ID`. (`CLOUDFLARE_ACCOUNT_ID` is a **var**, not a secret — both workflows have always read `vars.CLOUDFLARE_ACCOUNT_ID`; since #696 it is also substituted into the container image reference, so a missing value now fails the `wrangler containers push` step outright.)
- **Header budget — any proxy in front of the DS needs ≥ 8 KiB (#668):** request auth signs with the device's ML-DSA-44 key, so `X-Pollis-Signature` is base64 of a 2420-byte signature — **~3228 characters**, up from 88 in the Ed25519 era. That fits every default on the current path (hyper's 16 KiB per-header limit, Cloudflare's 16 KiB total), so nothing needs changing today — but it is a hard constraint on anything inserted in front of the DS later (an nginx `large_client_header_buffers`, an ALB, a self-hoster's reverse proxy): **configure no header budget below 8 KiB**, or every authenticated write 400s/431s.
- **Runs at:** **api.pollis.com** (prod) / **api-dev.pollis.com** (dev) via Worker custom domains (route change on the CF zone). Health: `/health`; build SHA: `/version`.
- **Dev is much slower than prod and NOBODY KNOWS WHY (#658 — open as a deferred spike).** On identical code and images, measured from the **same** Cloudflare edge (IAD, confirmed via `cf-ray`): prod `/version` ~47 ms vs dev ~280 ms — with **no database involved at all** — and a signed endpoint (one Turso read) ~65 ms vs ~1 400 ms. The dev Turso database answers in **~20 ms** queried directly, so the database is not the cause; the cost is somewhere between the edge and it.
  - **The placement hypothesis was tested and FAILED.** A named Durable Object is placed near whoever first instantiated it, permanently, and `getContainer(binding, name)` passes no hint (it is exactly `idFromName` + `get`) — so placement was never configured. The obvious theory was that dev's container simply landed far from `aws-us-east-1`. #658 tested it end to end: a fresh object created under `locationHint: "enam"`, reached via the staged `max_instances` migration below. **Result: no change at all** — ~280 ms and ~1.4 s afterwards, identical to before. A `locationHint` does **not** appear to govern where a *container-backed* object's container is scheduled, whatever it does for the object itself.
  - **Consequence for prod: its good latency is unexplained and NOT known to be reproducible.** Do not assume a recreated prod object would land as well. That is now the main argument for leaving `DS_SINGLETON_NAME` alone in prod — not that we can steer placement, but that we demonstrably cannot.
  - **dev runs at `max_instances: 2`, and that is not a capacity choice.** The failed experiment left dev with two durable objects — the orphaned original and the live `-enam` one — and the orphan still holds a container instance. A cap of 1 therefore starves the live object and every dev request fails with `Maximum number of running container instances exceeded`; this took dev down **twice** before the cause was understood. The cap is a ceiling, not a reservation, so the idle orphan costs nothing. **Return dev to 1 only after the orphaned object is genuinely deleted** (Cloudflare dashboard/API — wrangler has no delete for a durable object). Lowering it first breaks dev every time. prod is unaffected: one object, cap 1.
  - The hint is retained (`DS_LOCATION_HINT` in `worker/index.ts`) because declaring it is free and correct, **not** because it is a working performance lever. Do not reach for it expecting latency to move. Next steps live on the #658 spike — whether to stay on CF Containers at all, versus ECS/Fargate, persistent EC2, or bare metal.

  - **`DS_SINGLETON_NAME` is a PER-ENVIRONMENT wrangler `var`**, not a shared constant (`wrangler.{dev,prod}.jsonc`; the worker falls back to prod's name if unset). Object name determines identity, identity determines placement — so a name shared across environments would mean dev could not be re-placed without also re-placing **prod** on its next deploy. That coupling is how a routine dev migration turns into a production outage; keep them separate.
  - **Re-placing an EXISTING container needs a staged instance-cap migration — never a bare rename.** Bumping `DS_SINGLETON_NAME` creates a *second* durable object while `max_instances: 1` permits only one container instance, so the incoming object can never start. Tried on 2026-08-06 against dev; the deploy's verify step stalled and every request 500'd with `Failed to start container: Maximum number of running container instances exceeded` until traffic was routed back to the original name (~15 min of dev downtime; prod was not deployed and was unaffected). The DO is stateless — all DS state is in Turso — so the blocker is the cap, not data. The correct sequence, if a re-placement is ever actually wanted:
    1. Deploy with `max_instances: 2` and the **old** name (cap raised, nothing else changes).
    2. Deploy the new `DS_SINGLETON_NAME`. The new object starts alongside the old one; traffic moves to it.
    3. Let the orphaned object idle past `sleepAfter` (10 min) so it scales to zero and releases its instance.
    4. Deploy `max_instances: 1` again.
    Each step is its own deploy — verify `/version` between them.
  - **Rolling back a failed rename is fast:** redeploy the previous commit (`gh workflow run "Deploy Delivery (dev)" --ref <good-sha-branch>`). Routing returns to the surviving object; the orphan sleeps out on its own.
  - **History (#515):** replaced the old GHCR-build + VPS-Watchtower path (which silently no-op'd deploys for 11 days). Cut over 2026-07-08/09. The VPS `delivery`/`delivery-dev`/`watchtower` containers were kept stopped-but-present as a one-week rollback, then **torn down 2026-07-14** (#555): containers removed, local + GHCR delivery images deleted, stale nginx `api`/`api-dev` backups removed. The active nginx there had already dropped the api vhosts at cutover, and `api`/`api-dev.pollis.com` resolve to Cloudflare, so the VPS now runs the LiveKit stack only.
- **Role:** sole writer to Turso; clients hold read-only tokens and write only via the DS (structural commit-log integrity, #419/#420). Also the authorized-secrets broker (`/v1/livekit/*`, `/v1/r2/presign`, `/v1/turso/token` — #393).
- **Cold start / always-on (#515, #695):** `worker/index.ts` sets `sleepAfter = "10m"` — the container scales to zero after 10 min idle and the next request pays a cold boot. This is **not** a bug to "fix": `"10m"` is exactly the `@cloudflare/containers` default, deleting it is a no-op, and the library has **no "never sleep" value** (`parseTimeExpression` accepts only `<n>[smh]` or bare seconds). Always-on isn't expressible via `sleepAfter`; the only lever is a large finite window (e.g. `"24h"`) that costs money to keep an idle container warm. So the owner's call is **re-measure before buying it**.

    Note the earlier claim here — that #694/#717 "removed" the 2–4.5 s per-request Turso lookup — was **too strong, and wrong in exactly this context** (#658). Those changes added a `DeviceKeyCache`, which elides the auth read on a cache **hit**. A cold start begins with an **empty cache**, so the first signed request after the container scales to zero pays the full uncached lookup — the very case this bullet is about. What actually made prod fast is that its container sits ~20 ms from the database (see the placement bullet above), not that the lookup stopped happening.
  - **Measure this after the DS deploy carrying #716 + #717 is live** (`/version` shows that SHA). Quiet the service ≥ 15 min (> `sleepAfter`, so it's scaled to zero), then time a first request against a warm baseline:
    - Cold: `curl -o /dev/null -s -w '%{time_total}\n' https://api.pollis.com/health` — the first hit after the idle window (it triggers the container boot).
    - Warm: run the same curl again immediately (2nd/3rd hit) for the steady-state number.
    - Cold-start penalty = `cold − warm`. Take a few samples of each.
  - **Decision rule:** if the penalty is roughly ≤ 1 s, leave `sleepAfter = "10m"` — the occasional cold hit is cheaper than a 24/7 container and clients already retry the ~1–3 s deploy blip. If it's persistently multi-second (say ≥ 3 s) *and* it lands on real user traffic (first message after an idle spell), that's the number that justifies widening the window (`sleepAfter = "24h"`) or otherwise paying to keep it warm — a running-cost tradeoff, filed on #515.

### LiveKit + nginx — media SFU (voice / video / screenshare)
- **From:** `livekit/` (compose + nginx ingress; runs **upstream** LiveKit images, not our build)
- **Pipeline:** `.github/workflows/livekit-deploy.yml` — `workflow_dispatch` (env choice prod/dev). SSH + compose on the VPS.
- **Runs at:** VPS. Frames are E2EE (the SFU forwards ciphertext).

### Relay pool — closed-overlay first-party relay nodes
- **From:** `pollis-relay/` (the `pollis-relay` binary; shares the offline device-cert primitive `pollis-device-cert`)
- **Runs on:** **hosts with a public UDP port** — the **VPS/host model, like LiveKit, NOT Cloudflare Workers**: the overlay hop is QUIC (UDP), which a Worker/Container front-door can't terminate, and the relay terminates its *own* outer QUIC (a pinned self-signed identity) directly. Stateless and disposable — a node holds only its own QUIC identity keypair (persisted so restarts keep the same pinned cert; delete to rotate). **No Turso URL/token, no DS creds** — auth is the offline device-cert chain, so the relay is genuinely outside the metadata plane (design §11.1).
- **Image:** `.github/workflows/relay-image.yml` — **`workflow_dispatch`** (optional `ref`; also builds on PRs touching `pollis-relay/**` as a gate). Job 1 runs `cargo test -p pollis-relay -p pollis-device-cert`; Job 2 builds `pollis-relay/Dockerfile` (repo-root context, `GIT_SHA` baked) and publishes to **GHCR** `ghcr.io/actuallydan/pollis-relay` (multi-arch amd64+arm64). GHCR (not CF Containers, where the DS moved in #515) is deliberate: the relay is the VPS/host model, so a plain pullable image is the right shape. GHCR must be public (or the nodes given a pull secret).
  - **An image roll reaches the hydra fleet automatically (#703, resolves #677).** Nodes launch an **immutable, recorded** build (a digest pin), never `:latest` — a mutable tag never updates a running node (Docker caches by content hash; `--restart=always` never re-pulls), which used to split-brain the pool across two relay generations (and, because the relay bumps its ALPN with `PROTOCOL_VERSION`, clients that reached a wrong-generation node just failed QUIC ALPN). The intended build is recorded in the hydra's `intended-image` SSM param; after publishing, a final workflow job records it there via a **short-lived GitHub OIDC token** (role in `infra/relay-hydra/ci-oidc.tf`; **no standing AWS creds in CI**). The always-running reconciler then converges the fleet on its schedule: it **excludes wrong-protocol nodes from the signed directory immediately** and **cycles wrong-build nodes at a bounded rate** (details in `infra/relay-hydra/README.md`). **Half-roll safety:** if the record step fails/skips, the param is unchanged, so the whole fleet stays on the previous build — nothing is half-rolled. Operator-provisioned (non-hydra) hosts are still a manual `docker run` + roll.
  - **Required GHCR config:** the workflow pushes with the built-in `GITHUB_TOKEN` + `packages: write`. If the org restricts package writes, an admin must allow this repo's token to publish (Org → Packages), else Job 2's push fails while the tests/build still pass.
- **Runs at:** either operator-provisioned hosts (≥2 unrelated providers) **or** the automated AWS pool — **the hydra** (`infra/relay-hydra/`, #616): Terraform stands up a per-region mixed-instances ASG of `t4g.nano` Spot nodes (us-west-2 only, jurisdiction-filtered by US state) + S3/CloudFront-hosted signed directory + a reconciler Lambda that scales the pool, health-checks `/version`, and re-signs the directory each cycle. Hands-off after apply (edit an SSM desired-state param to scale), < $20/mo. Health/version: `GET /health` / `GET /version` on the configured `health_bind` TCP port; the QUIC relay listens on `bind` (`0.0.0.0:9444/udp` default). Turnkey (VPS) runbook: `docs/relay-operations.md`; automated AWS runbook: `infra/relay-hydra/README.md`.
- **Role:** the first-party fallback relay pool (design §7) that makes "messages must work" real with zero volunteer peers; hides the client source IP from Turso/DS/R2 (breach/subpoena defense, §11.1). Never part of the E2EE proof; only ever forwards sealed TLS bytes to a pinned first-party host.

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
| `mls-tests.yml` | DS serializer, MLS crypto/state-machine unit tests, multi-client integration flows harness + marathon soak (protects the bulletproof-membership invariants). The heavy `tests` job runs on any Rust/workspace change; frontend/website/docs/markdown-only PRs are exempt (decided by a seconds-cheap `changes` job, not `paths-ignore`, so the workflow always runs). A final `gate` job (`needs: tests`, `if: always()`) reports green on pass-or-exempt and red on failure/cancel — **`gate` is the merge-blocking required check** (#698). |
| `kani.yml` | Kani bounded-model-checking proofs on `pollis-core` + `pollis-delivery` pure fns (watermark no-skip, recovery gate, canonicalization, gap/head arithmetic). Path-filtered to `pollis-core/**`. |
| `supply-chain.yml` | cargo-deny (advisories/licenses/bans/sources) + cargo-vet (dependency review provenance). Runs on every PR. |
| `verifiable-log-tests.yml` | `cargo test` on the three `verifiable-log*` crates (transparency infra + `pollis-verify`). Pure Rust, no system deps. |
| `tla.yml` | TLC exhaustively model-checks both TLA+ specs — Spec A CommitLog (invariants I1+I2) + Spec B Delivery (I3+I4) — plus a "teeth" check that each broken variant still produces a counterexample. JVM-only, no Rust build. Path-filtered to `specs/tla/**`. |
| `e2e-smoke.yml` | WebDriver smoke of the **real Tauri binary**: does the app launch and show the login screen (`e2e/smoke.js`, no DS / shared-Turso dependency). `workflow_dispatch`-only — a full cargo build + virtual WebKitGTK window is too heavy for every push; run it before a release or after touching auth/bootstrap. |
| `mobile-core-check.yml` | Two cheap cross-compile gates (`android-check` / `ios-check`) build `pollis-core` for Android + iOS (aarch64, `--no-default-features`, matching the ubrn build) so mobile `#[cfg]` gate rot becomes red CI instead of a latent defect on the next mobile build. Plus (#706) an `android-build` job that assembles a real installable **APK** end to end (uniffi gen → 3-ABI `cargo-ndk` cross-compile → `expo prebuild` → Gradle release), uploaded as the `pollis-android-apk` artifact, and an `expo-doctor` config-health job. Path-filtered to `pollis-core/**` + `mobile/**` — never always-on. The APK is debug-keystore-signed only (Play upload signing is blocked, PL-18). |
| `frontend-check.yml` | Renderer typecheck on every PR to `main`: filtered pnpm install of `frontend/` only, then plain `tsc` (noEmit — no vite build, no artifacts). A frontend change can no longer merge without a typecheck. |

### Dispatch-only release-verification tooling (`workflow_dispatch`, take a released tag)

| Workflow | Does |
|---|---|
| `attest-release.yml` | Backfills the binary-transparency attest step for an **already-published tag** — no rebuild (~2 min vs a ~40 min release). Deliberately duplicates `desktop-release.yml`'s built-in `attest-and-log` job: same `scripts/attest-binaries.sh`, the tag's commit timestamp, the published release assets. Idempotent — a tag already in the accumulator is a no-op. |
| `rebuild-verify.yml` | The **third-party reproducer** (#484): rebuilds a released tag's Linux AppImage from public source with **no Pollis secrets** — runnable from a fork — and asserts the payload hash against the transparency log, trusting only the pinned log key. Log inclusion is verified in its own job (`verify-log-inclusion`), independent of the rebuild; bit-for-bit **reproduction** additionally needs the published build recipe supplied as non-secret repo `vars` (since #506 the only secret-shaped `option_env!` value left is the optional `LOG_DB_TOKEN` — see `docs/reproducible-builds-residuals.md`). |

---

## Quick "what do I deploy when I change X?"

| You changed… | Redeploy / re-release |
|---|---|
| `website/` | `website-deploy.yml` |
| `pollis-delivery/` (or `pollis-core` DS paths) | `delivery-deploy-dev.yml` → verify → `delivery-deploy-prod.yml` |
| `pollis-relay/` (or `pollis-device-cert`) | `relay-image.yml` (build + publish the GHCR image); then an operator rolls the pool nodes per `docs/relay-operations.md` — no auto-deploy |
| `livekit/` config | `livekit-deploy.yml` |
| `pollis-core` / `src-tauri` / `frontend` (desktop-facing, user-visible) | tag a new `v*` → `desktop-release.yml` (also releases the DB migrations) |
| `pollis-tui` / `pollis-core` (CLI-facing) | `cli-release.yml` |
| a DB migration in `pollis-core/src/db/migrations/` | applied by whichever runs first: a DS deploy (`delivery-deploy-{dev,prod}.yml`) or `desktop-release.yml` (`apply-migrations`) — both migrate-then-ship, idempotent; dev also auto-applies on merge via `db-migrate-dev.yml` |
| `verifiable-log*` (published-tree behavior) | `transparency-publish.yml` (also runs daily) |
| `verifiable-log-serve` (`pollis-verify` CLI) | `verifier-release.yml` |

---

## Post-merge release checklist — did anything downstream go stale?

**Run this after every merge that lands a real feature or fix — not just the ones
you plan to deploy.** The failure mode this prevents: you fix something in a
shared crate, ship *one* consumer (say the desktop app), and leave the DS, the
CLI, the verifier, or the transparency log running the old code for days without
noticing (this is literally what happened in #515 — the DS silently ran stale for
11 days). You do **not** rebuild everything on every merge. You **do** consciously
walk the outputs, decide deploy-or-not for each, and — for the ones you deploy —
**confirm the new build is actually live**, not merely that a workflow was fired.

### Step 1 — map the change to its blast radius

Most merges touch a **shared crate**, and the fan-out is wider than the directory
you edited. Use this, not intuition:

| You touched… | Consumers that can now be stale |
|---|---|
| `pollis-core/` | **Desktop** (`src-tauri`+`frontend`), **CLI** (`pollis-tui`), **Mobile** (uniffi). If you changed a **wire contract** — canonical signing, `livekit_jwt`, `r2` presign, DS request/response shapes, MLS envelope encoding — **also the DS**: `pollis-delivery` re-implements that scheme **by hand** and does *not* compile against `pollis-core`, so the compiler will **not** catch drift. Mirror the change into `pollis-delivery` and redeploy it. |
| `verifiable-log*` | `pollis-core` depends on `verifiable-log-serve`, so the change ripples into **every client** (desktop/CLI/mobile) **and** the **`pollis-verify` CLI** (`verifier-release.yml`) **and** the published **transparency tree** (`transparency-publish.yml`). |
| `pollis-delivery/` | **DS** only (dev → verify → prod). |
| `pollis-relay/` / `pollis-device-cert/` | **Relay pool** only — rebuild the GHCR image (`relay-image.yml`), then roll the pool nodes. `pollis-device-cert` is shared with `pollis-core` (it mints the certs the relay verifies), so a change there also fans out to every **client** — keep the mint/verify halves in lockstep (golden vector). |
| `frontend/` / `src-tauri/` | **Desktop** only. |
| `pollis-tui/` | **CLI** only. |
| a DB migration in `pollis-core/src/db/migrations/` | Applied by whichever prod deploy runs first (a DS deploy **or** `desktop-release.yml`) — migrate-then-ship, idempotent. If the merge ships **neither** a client release nor a DS deploy, prod has **not** run the migration yet; note it as pending. Dev auto-applies via `db-migrate-dev.yml`. |
| `website/` / `livekit/` | **Website** / **LiveKit stack** only. |

### Step 2 — for each output in the blast radius, decide and record

For every affected output below, pick one: **`— redeploy` / `— defer (reason)` / `— N/A`**. Deferring is fine; *silently forgetting* is the bug. Record the decision in the PR description or merge comment so the next person can see nothing was dropped.

- [ ] **Desktop app** — user-visible change? tag a new `v*` → `desktop-release.yml` (also applies pending migrations).
- [ ] **CLI (`pollis`)** — behavior a terminal user sees? `cli-release.yml` (`workflow_dispatch`). Easy to forget after a `pollis-core` change.
- [ ] **DS** — DS code *or* a mirrored wire-contract change? `delivery-deploy-dev.yml` → verify → `delivery-deploy-prod.yml`.
  - **Ordering (sealed sender / client-side edit-delete authz, #607):** the DS
    authz change (drop the edit/delete author checks; keep membership + admin
    gates) **must be live in prod BEFORE any client that seals** ships. A sealing
    client posts `sealed = 1` with the sentinel `sender_id`; an old DS still
    running the author-equality check compares the sentinel to the real editor and
    **403s every edit and self-delete**. The order is manageable because DS deploys
    are `workflow_dispatch` while client sealing rides a desktop release — but it is
    **not automatic**: deploy the DS, confirm `/version` shows the merged SHA, and
    only then cut the client release. There is **no data migration** for this
    cutover — sealing was never previously on, so there are no unsealed rows to
    convert; new sends simply start sealing.
  - **Ordering (post-quantum authentication, #668): the DS, the relay pool and
    the client ship in the *same* cycle — DS first, relay pool before the
    client.** Four things move together. (1) Migration
    `000011_device_pq_signature_pub.sql` adds the nullable
    `user_device.mls_signature_pub_pq` column — migrate-then-ship means whichever
    prod deploy runs first applies it, and it must be applied before either side
    is live. (2) `POST /v1/auth/publish-device-cert` now takes
    `mls_signature_pub_pq` and re-derives the **v2** cert payload over *both* leaf
    keys, so an old DS rejects a new client's cert and an old client's body is
    missing a field the new DS requires. (3) DS request auth verifies
    `X-Pollis-Signature` as ML-DSA-44 against `mls_signature_pub_pq`, so a new
    client's writes are unauthenticatable by an old DS and an old client's
    Ed25519-signed writes are unauthenticatable by a new one. (4) **The relay
    handshake is a fourth moving part, and it is easy to miss because it is not
    the DS.** The frame swapped its fixed-width Ed25519 slots for ML-DSA-44 ones
    (`[32]`/`[64]` → `[1312]`/`[2420]`), so `PROTOCOL_VERSION` went 2 → 3, ALPN
    `pollis-relay/2` → `/3`, and `HANDSHAKE_DOMAIN` `pollis-relay-v2` → `-v3`.
    Because ALPN is negotiated before a single handshake byte is written, a
    version straddle fails *closed and fast* rather than hanging — but it still
    means a post-#668 client cannot use a pre-#668 node **at all**. Roll every
    pool node onto the new image and confirm each `GET /version` before cutting
    the client release. On the hydra pool (post-#703) a node runs the **recorded
    intended build**, not `:latest`, and the reconciler cycles wrong-build nodes
    automatically once CI records the new image — but a wire-breaking bump ALSO
    needs `expected_relay_protocol` set to the new ALPN so wrong-generation nodes
    are excluded from the directory: publish + converge the fleet onto the new
    image first, then bump the expected protocol (`infra/relay-hydra/README.md` →
    "Roll the relay image"). Deploy the DS (dev → verify → prod, confirm `/version`), roll the relays,
    then cut the client release — and do not leave the versions straddling a
    release cycle. The PQ MLS suite's code point also moved **in place**
    (`0x004D` → `0x0052`), so a pre-#668 client cannot read a post-#668 group's
    traffic at all.
  - **Ordering (classic suite retirement, #669): DS first, then the client, same
    cycle.** There is **no migration** — no column added, none dropped;
    `user_device.pq_capable` is retired in place and `mls_key_package.ciphersuite`
    keeps its SQL default of `1`, because migrations must stay additive. What
    changes on the wire is the DS's fallback: a publish or claim that omits the
    `ciphersuite` field now defaults to `CIPHERSUITE_PQ` (`0x0052`) instead of the
    classic `1`. Post-#669 clients always send the field explicitly, so the
    fallback only bites requests from an older client, which would land its classic
    pool in the PQ bucket. Deploy the DS (dev → verify → prod, confirm `/version`),
    then cut the client release. This was a hard cutover rather than a fleet-turnover
    wait because the deployment has no active users; on a fleet with real users the
    #454 capability gates would still be required.
- [ ] **Relay pool** — `pollis-relay` / `pollis-device-cert` change? rebuild the GHCR image (`relay-image.yml`). On the **hydra pool** (post-#703) that workflow also records the new immutable build into the `intended-image` SSM param (GitHub OIDC, no standing creds) and the reconciler converges the fleet on its schedule — verify each `GET /version` reports the new `sha` and the CloudWatch `StaleBuildNodes` metric returns to 0. Operator-provisioned (non-hydra) hosts are still a manual roll per `docs/relay-operations.md`. A wire-breaking protocol bump additionally needs `expected_relay_protocol` bumped (coordinated release — see above).
- [ ] **Mobile** — in development; not a released output yet, but note if a `pollis-core` change needs a `#[cfg]`/uniffi follow-up. `mobile-core-check.yml` gates both `#[cfg]` gate-rot (cross-compile) **and** the full Android build (`android-build` assembles the APK) — a `pollis-core` change that breaks the uniffi surface, the prebuild, or the Gradle assembly now shows up as red CI, not on the next workstation build.
- [ ] **pollis-verify CLI** — `verifiable-log*` change affecting verification? Bump `version` in `verifiable-log-serve/Cargo.toml`, then tag `pollis-verify-v<that version>` to fire `verifier-release.yml`. The workflow refuses to release if the two disagree (#670).
- [ ] **Transparency log** — tree/STH behavior changed? it rebuilds daily, but force `transparency-publish.yml` if correctness depends on it now.
- [ ] **DB migration** — pending on prod until a client release or DS deploy runs? Note it, or trigger one.
- [ ] **Website / LiveKit** — only if you touched `website/` / `livekit/`.

### Step 3 — verify the deploy actually landed (don't trust "workflow started")

A green workflow is **not** proof the new bits are serving traffic. For anything you did deploy:

- **DS** — poll `/version` for the merged git SHA (the #509 tripwire the deploy already does; check it). `/health` alone is not enough.
- **Desktop / CLI** — the new version appears under `cdn.pollis.com/releases/<version>/…` and in the `latest.json` / `cli/latest.json` manifest.
- **Website** — the change is live on **pollis.com** (Cloudflare Pages deploy is a manual button; `main` being green ≠ deployed).
- **Transparency log** — `verify.pollis.com` STH advanced; `pollis-verify` still passes.
- **Migration** — confirm it's recorded in prod `schema_migrations` after the deploy that was supposed to apply it.

If a target is deferred, it is now **known-stale** — say so, and make sure the eventual deploy that ships it is on someone's radar.
