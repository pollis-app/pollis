# E2E tests (real Tauri app, driven via WebDriver)

Drives the **actual native desktop app** — the real WebKitGTK WebView inside the
Tauri shell, talking to the real Rust core over Tauri IPC — not the browser
build of the frontend. Proves the flow only the shipped shell can exercise:
launch → create account → log in → screenshot.

```bash
pnpm --filter @pollis/e2e test      # or: node e2e/e2e.js
```

Proof screenshots land in `e2e/artifacts/` (`01-auth-screen.png`,
`99-app-ready.png`); on failure it writes `FAIL.png` + `FAIL.html` and prints
the testids that were actually on screen.

## How it works

`e2e.js` stands up the whole local stack, then drives the app with raw
`webdriverio` `remote()` calls:

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

  This matters: `pollis-core/src/config.rs` reads env with `option_env!`,
  which **bakes values in at compile time** and beats the runtime environment.
  A binary built with `.env.development` sourced has `api-dev.pollis.com`
  baked in and will silently ignore the local-DS override.

- **`.env.test`** must point at a **writable, disposable** Turso DB with the
  schema applied. `.env.development`'s Turso token is read-only — signup fails
  on it — which is why all Turso access is redirected to the test DB.

  > **Schema bootstrap is a manual, one-time step** (see "Known limitation"
  > below). The integration test harness already bootstraps `.env.test`
  > idempotently, so the simplest way to get a current schema is to run it once:
  >
  > ```bash
  > cargo test --features test-harness --test flows
  > ```
  >
  > (Any single scenario is enough — it stamps the baseline + all migrations
  > into the DB.) Alternatively, apply migrations by hand against a fresh DB:
  >
  > ```bash
  > set -a; . .env.test; set +a
  > for f in pollis-core/src/db/migrations/0*.sql; do
  >   turso db shell "$TURSO_URL" < "$f"
  > done
  > ```
  >
  > If a run fails with `no such table: …`, the test DB is behind on migrations
  > — re-run the harness (or apply the missing file).

## Known limitation — no automatic schema bootstrap

`e2e.js` does **not** bring the test DB's schema up itself; it assumes the
schema is already applied (see above). Auto-bootstrapping was deliberately
deferred: the `pollis-core` test harness — the source of truth for schema
setup — is under active change, and duplicating its migration logic here now
would create churn/conflicts. Once that settles, `e2e.js` should apply the
baseline + numbered migrations at startup (reading `pollis-core/src/db/
migrations/*.sql`, gated on `schema_migrations`) so a fresh clone / CI works
with no manual step. Until then this harness is **not** fully self-contained
for CI.

## Gotchas encoded in e2e.js (learned the hard way)

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
