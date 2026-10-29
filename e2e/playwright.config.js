// Playwright config for the UI specs: browser build with Tauri mocked (VITE_PLAYWRIGHT=true), no backend.
const { defineConfig, devices } = require("@playwright/test");

// Per-checkout port (POLLIS_E2E_PORT overrides): a fixed one plus reuseExistingServer serves another worktree.
const crypto = require("node:crypto");
const PORT =
  Number(process.env.POLLIS_E2E_PORT) ||
  5174 +
    (parseInt(
      crypto.createHash("sha1").update(__dirname).digest("hex").slice(0, 6),
      16,
    ) %
      300);

module.exports = defineConfig({
  testDir: __dirname,
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
