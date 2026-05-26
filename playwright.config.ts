import { defineConfig } from "@playwright/test";

// E2E tests drive the real Electron binary via Playwright's `_electron`
// API, with `pollis-node` (debug) loaded into the main process. Each test
// gets its own isolated `POLLIS_DATA_DIR` so the local SQLite + accounts
// index + keystore JSON-file backend never collide. All tests share the
// disposable Turso instance configured in `.env.test` at the repo root —
// the same one the Rust integration harness uses. Tests serialise on
// the Turso wipe in `tests/e2e/helpers/turso.ts`, so we run a single
// worker and rely on per-test wipes for isolation.

export default defineConfig({
  testDir: "tests/e2e",
  // Per-test wipes of the shared Turso DB make parallel workers unsafe.
  workers: 1,
  fullyParallel: false,
  // The Rust integration harness picks ~3–4 min worst-case; Electron
  // launches add another 5–10 s each. Allow a generous wall clock.
  timeout: 120_000,
  // Each `expect(...).toPass()` / `toBeVisible()` poll deadline. The MLS
  // round-trip through Turso (insert envelope → poll → decrypt) can take
  // several seconds; keep this above the default.
  expect: { timeout: 15_000 },
  // The Electron binary's first launch builds nothing — the npm scripts
  // do that. But Playwright's worker still needs a stable boot window.
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: [
    ["list"],
    ...(process.env.CI ? ([["github"]] as const) : []),
  ],
  use: {
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  // The Electron main process loads the renderer from
  // `http://localhost:5173` in dev mode (its only non-packaged path).
  // Rather than teach main.ts about tests, the harness spins up a
  // `vite preview` serving the built frontend on that port. Playwright
  // owns the server lifecycle — it starts before the suite, gets killed
  // when the suite ends, and waits up to 60 s for the port to bind.
  webServer: {
    command: "pnpm --filter frontend exec vite preview --port 5173 --strictPort",
    url: "http://localhost:5173",
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    stdout: "ignore",
    stderr: "pipe",
  },
});
