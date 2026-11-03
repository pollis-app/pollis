// Profile picture: removing your own (parity with mobile's Remove photo). An empty key reads as none.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const HASH = "ab".repeat(32);
const KEY = `avatars/${USER.id}/${HASH}.jpg`;
// A 1x1 PNG, served for the key the way the loopback server would.
const PIXEL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

async function boot(page: Page, avatarUrl?: string) {
  const state = {
    session: USER,
    profile: { id: USER.id, username: USER.username, email: USER.email, avatar_url: avatarUrl },
    preferences: JSON.stringify({ skin: "refined" }),
    publicFileUrls: { [KEY]: PIXEL },
  };
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
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
  }, state);
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openUserSettings(page: Page) {
  await page.getByTestId("breadcrumb-settings-button").first().click();
  await page.getByTestId("menu-item-user").click();
  await expect(page.getByTestId("settings-page")).toBeVisible();
}

test("your avatar can be removed, and stays removed after the profile refetches", async ({ page }) => {
  await boot(page, KEY);
  await openUserSettings(page);

  await expect(page.getByTestId("avatar-current")).toBeVisible();
  await expect(page.getByTestId("sidebar-profile-avatar").locator("img")).toHaveCount(1);
  await page.getByTestId("remove-avatar-button").click();

  await expect(page.getByTestId("avatar-placeholder")).toBeVisible();
  await expect(page.getByTestId("avatar-remove-success")).toHaveText("Avatar removed.");
  await expect(page.getByTestId("remove-avatar-button")).toHaveCount(0);

  // The refetched profile carries the empty key, which draws nothing anywhere.
  await expect(page.getByTestId("sidebar-profile-avatar").locator("img")).toHaveCount(0);
});

test("with no avatar there is nothing to remove", async ({ page }) => {
  await boot(page);
  await openUserSettings(page);
  await expect(page.getByTestId("avatar-placeholder")).toBeVisible();
  await expect(page.getByTestId("remove-avatar-button")).toHaveCount(0);
});
