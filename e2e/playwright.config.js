// Playwright config for the UI-level specs (`e2e/*.spec.js`).
//
// These drive the BROWSER build of the frontend with `VITE_PLAYWRIGHT=true`,
// which vite-aliases `@tauri-apps/api/*` to the mocks in
// `frontend/src/__mocks__/`. That is the whole point: a composer-interaction
// test needs the real React tree and the real CSS tokens, but nothing from
// MLS, the delivery service, or Turso — so it runs anywhere, with no backend.
//
// The WebDriver scenarios (`e2e/*.js`, run via `pnpm e2e <name>`) are a
// different tool for a different job: they drive the real native Tauri shell
// end-to-end. Neither replaces the other.
const { defineConfig, devices } = require("@playwright/test");

const PORT = 5174;

module.exports = defineConfig({
  testDir: __dirname,
  // Both dialects: mentions (#843) wrote .spec.js, bookmarks (#854) wrote
  // .spec.ts. Playwright handles TS natively, so one config runs both rather
  // than the repo carrying two configs and two script names.
  testMatch: /.*\.spec\.(js|ts)$/,
  outputDir: `${__dirname}/artifacts/playwright`,
  // Composer interaction is keystroke-by-keystroke; give it room on a cold
  // dev-server transform without being generous enough to hide a hang.
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    // `exec vite` rather than `dev --`: pnpm swallows the `--` separator and
    // vite ends up back on its default port. `--host 127.0.0.1` is load-
    // bearing too — vite's default `localhost` binds IPv6-only on some Linux
    // boxes, which leaves the v4 baseURL below permanently unreachable.
    command: `pnpm --filter frontend exec vite --port ${PORT} --strictPort --host 127.0.0.1`,
    url: `http://127.0.0.1:${PORT}`,
    cwd: `${__dirname}/..`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
    env: { VITE_PLAYWRIGHT: "true" },
  },
});
