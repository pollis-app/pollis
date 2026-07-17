# E2E tests (real Tauri app, driven via WebDriver)

Drives the **actual native desktop app** — the real WebKitGTK WebView inside the
Tauri shell, talking to the real Rust core over Tauri IPC — not the browser
build of the frontend.

Three scripts, sharing `lib/harness.js` for the tauri-driver/WebKitWebDriver
plumbing:

| script | what it proves | needs DS? | needs Turso? |
|---|---|---|---|
| `smoke.js` | app launches, login screen renders | no | no |
| `e2e.js` | full signup: email → OTP → secret key → PIN → app-ready | yes | yes (writable) |
| `invalid-otp.js` | a wrong OTP code is rejected with an inline error, doesn't advance | yes | yes (writable) |

```bash
pnpm --filter @pollis/e2e smoke        # or: node e2e/smoke.js       (fast, no deps)
pnpm --filter @pollis/e2e test         # or: node e2e/e2e.js         (full signup flow)
pnpm --filter @pollis/e2e invalid-otp  # or: node e2e/invalid-otp.js
```

`smoke.js` is the one to reach for in CI or as a quick "did I break launch"
check — `checkStoredSession()` (`frontend/src/App.tsx`) resolves the
logged-out path entirely from local Tauri commands (`getSession` /
`listKnownAccounts`), so it never touches the delivery service or Turso. The
other two need the local delivery service + a writable disposable Turso DB
(see Prerequisites below).

Proof screenshots land in `e2e/artifacts/` (`smoke-auth-screen.png`,
`01-auth-screen.png`, `99-app-ready.png`, `invalid-otp-error.png`); on failure
any script writes `FAIL.png` + `FAIL.html` and prints the testids that were
actually on screen.

## How it works

Each script stands up its own local stack (`smoke.js` skips the delivery
service entirely), then drives the app with raw `webdriverio` `remote()`
calls via helpers in `lib/harness.js` (`reap`, `waitPort`, `waitTestId`,
`clickTestId`, `typeCode`, `makeShot`, ...):

1. **Vite dev server** on `:5173`. The debug Tauri binary loads its UI from
   `devUrl` (Tauri only embeds the frontend in release builds — and even this
   repo's release profile keeps `devUrl`). Running the real dev server also sets
   `import.meta.env.DEV`, which skips the launch-time auto-updater gate in
   `App.tsx`. The script pre-warms Vite's lazy module transforms with `curl`
   (a plain Node request gets 404 from Vite 3) so the app's one-shot page load
   never hits a cold server.
2. **Local delivery service** (`pollis-delivery`) on `:8788` with
   `DEV_OTP=000000` and no `RESEND_API_KEY` — OTP email is skipped and the
   fixed code `000000` always verifies. Writes go to the disposable test DB.
3. **tauri-driver** (`:4444`) → **WebKitWebDriver** (`:4445`) → launches
   `target/debug/pollis` with: dev creds for R2/LiveKit, the **writable test
   DB** (`.env.test`) for Turso, `POLLIS_DELIVERY_URL` → the local DS, and —
   critically — `WEBKIT_DISABLE_COMPOSITING_MODE=1 GDK_BACKEND=x11`, same as
   `pnpm dev`. Without those, WebKitGTK compositing wedges the WebView on this
   setup: pages half-render and the screenshot endpoint hangs forever.

Nothing talks to prod. No OTP email is ever sent.

## Prerequisites (one-time)

- **System**: `WebKitWebDriver` (ships with webkit2gtk), a display, and
  `cargo install tauri-driver`.
- **Node**: `pnpm install` at the repo root. (webdriverio v8 — v9's undici
  client can't talk to tauri-driver.)
- **Binaries, built with a CLEAN environment**:

  ```bash
  env -u POLLIS_DELIVERY_URL -u TURSO_URL -u TURSO_TOKEN cargo build -p pollis -p pollis-delivery
  ```

  `smoke.js` only launches `target/debug/pollis` (no DS calls before the
  login screen renders), so `-p pollis` alone is enough for it. `e2e.js` and
  `invalid-otp.js` need `pollis-delivery` too.

  This matters: `pollis-core/src/config.rs` reads env with `option_env!`,
  which **bakes values in at compile time** and beats the runtime environment.
  A binary built with `.env.development` sourced has `api-dev.pollis.com`
  baked in and will silently ignore the local-DS override.

- **`.env.test`** — only needed for `e2e.js` / `invalid-otp.js` (`smoke.js`
  reads neither TURSO_URL nor POLLIS_DELIVERY_URL). Must point at a
  **writable, disposable** Turso DB with the
  schema applied. `.env.development`'s Turso token is read-only — signup fails
  on it — which is why all Turso access is redirected to the test DB.

  > **Schema bootstrap is a manual, one-time step** (see "Known limitation"
  > below). Note the Rust `flows` integration harness does **not** help here
  > as of `8b41317` (2026-05-05) — it runs against an embedded local libsql
  > file, not `.env.test`'s real Turso, so running it does nothing for this
  > DB's schema. Use the CI migration runner directly instead:
  >
  > ```bash
  > set -a; . .env.test; set +a
  > bash scripts/db-apply.sh
  > ```
  >
  > It's idempotent (tracks applied versions in `schema_migrations`) and is
  > exactly what CI runs before tests. If a run fails with "duplicate column"
  > or similar on a migration `db-apply.sh` thinks is pending, the DB's
  > `schema_migrations` bookkeeping has drifted from its actual schema
  > (columns/tables applied by hand outside the script at some point) —
  > diagnose with a manual `SELECT sql FROM sqlite_master WHERE name = '…'`
  > against the DB before assuming the migration file itself is wrong.

The scripts still fall back to `.env.test` for a purely local run (see
Prerequisites), but the backend fixtures below are the supported path — they
apply the schema for you and run the DS, so you don't hand-provision anything.

## Automatic backend fixtures (`start-backend.sh` / `stop-backend.sh`)

`e2e/scripts/start-backend.sh` (M1 of #570) stands up the whole backend the
authenticated scripts need — no hand-provisioned Turso, no manual schema step:

1. a real **libsql server** (Turso `libsql-server` / sqld) on `127.0.0.1:8080`,
   in no-auth local mode (so `TURSO_TOKEN` is an ignored placeholder);
2. the **schema**, applied by the repo's real migration runner
   (`scripts/db-apply.sh` over `/v2/pipeline`) — the same thing prod CI runs, so
   there's no second copy of the migration logic to drift;
3. the **real `pollis-delivery` binary** on `127.0.0.1:8788` with
   `DEV_OTP=000000` and Resend disabled (same knobs as the in-process DS in
   `src-tauri/tests/flows/harness.rs`, just the shipped binary).

It then exports `TURSO_URL` / `TURSO_TOKEN` / `POLLIS_DELIVERY_URL` /
`R2_S3_ENDPOINT` / `R2_PUBLIC_URL` / `DEV_OTP` — to `$GITHUB_ENV` in CI and as
`export …` lines on stdout locally. When `POLLIS_DELIVERY_URL` is already set,
`e2e.js` / `invalid-otp.js` use that external DS instead of spawning their own.

Needs Docker (for the libsql image) + `jq`/`curl` (in `scripts/db-apply.sh`).
The R2 values are unreachable **placeholders**: signup never dials R2, but
`Config::from_env()` requires the vars to be present. Real object storage
(MinIO/R2) and LiveKit/media come in later milestones (M3), when a
media/attachment test actually needs them.

### Run the full authenticated flow locally

```bash
# Build both binaries with a CLEAN env (see Prerequisites — option_env! bakes
# compile-time values that would beat the runtime overrides otherwise):
env -u POLLIS_DELIVERY_URL -u TURSO_URL -u TURSO_TOKEN cargo build -p pollis -p pollis-delivery

# Bring up libsql + schema + the real DS, and pull its exported env into your shell:
eval "$(e2e/scripts/start-backend.sh)"

# Drive the app against that backend:
node e2e/e2e.js          # full signup -> app-ready
node e2e/invalid-otp.js  # wrong OTP is rejected

# Tear it all down (idempotent):
e2e/scripts/stop-backend.sh
```

## CI history — schema bootstrap used to be manual

Before M1, `e2e.js` / `invalid-otp.js` did **not** bring the test DB's schema up
themselves — they assumed a hand-applied schema, which is why only `smoke.js`
(no schema dependency) ran in CI. `start-backend.sh` closes that gap by running
`scripts/db-apply.sh` against a fresh libsql server, so both scripts now run in
CI via `.github/workflows/e2e-full.yml` (below). `smoke.js` remains the fast,
backend-free launch check.

## Gotchas encoded in these scripts (learned the hard way)

- **Clicks**: WebKitWebDriver's native `element.click()` doesn't reliably fire
  React handlers/form submits here; `clickTestId()` dispatches a DOM click via
  `execute()`. Text inputs work fine with `setValue` (real keystrokes).
- **OTP / PIN**: N separate `<input maxlength=1>` boxes with
  `aria-label="OTP digit K"`; the widget auto-advances and auto-submits.
- **Settle before first command**: the script pauses ~6s after session create
  so the initial Vite page load finishes before the first WebDriver query.
- **Screenshots are time-boxed** (25s race) — a wedged compositor hangs the
  endpoint; the run continues without the shot rather than dying.
- **Orphan reaping**: a run killed mid-session leaves tauri-driver /
  WebKitWebDriver / `pollis` / `pollis-delivery` / vite behind; the next
  session then fails with "Maximum number of active sessions" or a 4445 bind
  error. `e2e.js` reaps all of these on start and on teardown.
- Vite 3 binds IPv6 loopback only (`[::1]:5173`); port checks probe both
  families.
- **`smoke.js` still needs placeholder env vars**: `Config::from_env()`
  (`pollis-core/src/config.rs`) hard-requires `TURSO_URL` / `TURSO_TOKEN` /
  `R2_S3_ENDPOINT` / `R2_PUBLIC_URL` to be present — baked in at compile time
  or set at runtime — or the app panics in its Tauri setup hook before any
  window opens. `smoke.js` supplies unreachable placeholders for these
  (`REQUIRED_PLACEHOLDERS`), since the login screen never dials any of them.
  If `Config::from_env()` grows a new required field, `smoke.js` needs a
  matching placeholder or it starts failing for an unrelated reason.

## Run the environment locally

The whole system environment this suite needs (WebKitGTK, the media/audio libs
pollis-core links, the pinned Rust toolchain, `tauri-driver`, `meson`, `xvfb`)
is packaged as a reusable Docker image — `e2e/Dockerfile` — so you can run the
smoke on any plain Docker host without hand-installing WebKitGTK. This is the
**single definition of the environment**; CI (`e2e-smoke.yml`) and later
milestones (backend fixtures, two-client, media) extend the same image and the
same shared setup rather than redefining it.

It is a **build-environment** image: the source is *not* baked in — you mount
your checkout at run time, so one image works across branches without a rebuild.

```bash
# Build the image once (from the repo root):
docker build -f e2e/Dockerfile -t pollis-e2e .

# Run the smoke inside it, mounting the current checkout:
docker run --rm -v "$PWD":/work -w /work pollis-e2e bash -lc '
  pnpm install --frozen-lockfile &&
  cargo build -p pollis &&
  dbus-run-session -- xvfb-run --auto-servernum pnpm --filter @pollis/e2e smoke
'
```

The image bakes the WebKit env gotchas (`WEBKIT_DISABLE_COMPOSITING_MODE=1`,
`GDK_BACKEND=x11`, `XDG_SESSION_TYPE=x11`), so a bare `docker run` inherits
them. The `dbus-run-session -- xvfb-run --auto-servernum …` prefix mirrors
exactly what CI does — it's the same wrapping the `desktop-e2e` composite
action applies (see below), so local and CI runs are byte-for-byte the same
environment.

Where the pieces live (all shared between the image and CI, so nothing is
duplicated):

| piece | file | used by |
|---|---|---|
| system deps (the apt list) | `e2e/scripts/install-system-deps.sh` | `e2e/Dockerfile` **and** the CI workflow |
| the whole build environment | `e2e/Dockerfile` | local Docker runs (and later milestones) |
| run-time wrapping (dbus + xvfb + orphan reaping) | `.github/actions/desktop-e2e/action.yml` | `e2e-smoke.yml` |

## CI

`.github/workflows/e2e-smoke.yml` runs `smoke.js` on Linux, triggered
manually (`workflow_dispatch`) — not on every push, since it needs a real
(virtual) display and a full `cargo build`, both slower than the rest of CI.
Trigger it from the Actions tab when you want a smoke check that the app
still launches. It installs the system deps via the shared
`e2e/scripts/install-system-deps.sh` (`webkit2gtk` + `webkit2gtk-driver` and
the media libs) and `tauri-driver`, then runs the smoke through the
`.github/actions/desktop-e2e` composite action, which wraps it in a dbus
session + `xvfb-run` and reaps orphan processes before and after.

`.github/workflows/e2e-full.yml` (issue #570, M1) runs the authenticated
scripts — `e2e.js` and `invalid-otp.js` — also `workflow_dispatch`-only. It
builds `pollis` **and** `pollis-delivery`, brings the backend up with
`e2e/scripts/start-backend.sh` (libsql + schema + real DS), runs both scripts
through the same `desktop-e2e` composite action, and tears the backend down in
an `if: always()` step. No nightly schedule (cost); trigger it from the Actions
tab when you want the full signup path exercised end-to-end.
