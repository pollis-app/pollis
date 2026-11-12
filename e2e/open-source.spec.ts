// Open source licences (#1832): Settings → Open Source Licenses lists every
// third-party package with its licence and opens each licence text. The browser
// build serves frontend/src/__mocks__/oss-notices.json in place of the
// generated list (vite-plugin-oss-notices.ts).

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

async function boot(page: Page) {
  await page.addInitScript(
    (state) => {
      (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = state;
      (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: "main" },
          currentWebview: { windowLabel: "main", label: "main" },
        },
        plugins: {},
        transformCallback: () => 0,
        convertFileSrc: (path: string) => path,
        registerListener: () => {},
        unregisterListener: () => {},
        runCallback: () => {},
        invoke: () => Promise.resolve(null),
      };
    },
    {
      session: USER,
      profile: { id: USER.id, username: USER.username },
      preferences: JSON.stringify({ skin: "terminal" }),
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

test("Settings opens the licence list, and a package opens its licence text", async ({ page }) => {
  await boot(page);
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("Settings");
  await page.locator('[data-testid="search-panel-result-item"][data-item-id="page-settings-hub"]').click();
  await expect(page.getByTestId("settings-hub-page")).toBeVisible();

  await page.getByTestId("menu-item-open-source").click();
  await expect(page.getByTestId("open-source-page")).toBeVisible();
  await expect(page.getByTestId("open-source-count")).toContainText("3");
  for (const name of ["react", "serde", "uniffi"]) {
    await expect(page.getByTestId(`open-source-row-${name}`)).toBeVisible();
  }
  await expect(page.getByTestId("open-source-row-serde")).toContainText("MIT OR Apache-2.0");

  await page.getByTestId("open-source-row-serde").click();
  await expect(page.getByTestId("open-source-license-page")).toBeVisible();
  await expect(page.getByTestId("open-source-version")).toHaveText("1.0.228");
  await expect(page.getByTestId("open-source-license")).toHaveText("MIT OR Apache-2.0");
  // Both licence files the crate ships, in order.
  const texts = page.getByTestId("open-source-text");
  await expect(texts).toHaveCount(2);
  await expect(texts.nth(0)).toContainText("serde, MIT");
  await expect(texts.nth(1)).toContainText("serde, Apache-2.0");
});

test("the filter matches a package name or a licence", async ({ page }) => {
  await boot(page);
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("open source");
  await page.locator('[data-testid="search-panel-result-item"][data-item-id="page-open-source"]').click();
  await expect(page.getByTestId("open-source-page")).toBeVisible();

  await page.getByTestId("open-source-filter").fill("mpl");
  await expect(page.getByTestId("open-source-row-uniffi")).toBeVisible();
  await expect(page.getByTestId("open-source-row-react")).toHaveCount(0);
  await expect(page.getByTestId("open-source-row-serde")).toHaveCount(0);

  await page.getByTestId("open-source-filter").fill("nothing-matches-this");
  await expect(page.getByTestId("open-source-row-uniffi")).toHaveCount(0);
});
