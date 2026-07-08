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

## Known limitation — no automatic schema bootstrap

`e2e.js` / `invalid-otp.js` do **not** bring the test DB's schema up
themselves; they assume the schema is already applied (see above) — that's
why CI (`.github/workflows/e2e-smoke.yml`) only runs `smoke.js`, which has no
schema dependency at all. Auto-bootstrapping was deliberately deferred:
duplicating `scripts/db-apply.sh`'s migration logic here now would create
churn/conflicts. Until that lands, `e2e.js` and `invalid-otp.js` are for
local use against a schema you've applied by hand; only `smoke.js` is
self-contained enough for CI.

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

## CI

`.github/workflows/e2e-smoke.yml` runs `smoke.js` on Linux, triggered
manually (`workflow_dispatch`) — not on every push, since it needs a real
(virtual) display and a full `cargo build`, both slower than the rest of CI.
Trigger it from the Actions tab when you want a smoke check that the app
still launches. It installs `webkit2gtk` + `webkit2gtk-driver`, `tauri-driver`,
and wraps the run in `xvfb-run` for a virtual X server. It does not run
`e2e.js` / `invalid-otp.js` — those need the shared writable test Turso,
which isn't provisioned in CI yet.
