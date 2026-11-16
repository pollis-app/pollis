/** One synced colour preference for desktop and mobile (design/theme.ts): shared keys, shared presets, one red. */

import { test, expect, type Page } from "./lib/zero-loss";

const ALICE = { id: "u-alice", email: "alice@example.com", username: "alice" };

async function boot(page: Page, preferences: Record<string, unknown>) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, {
    session: ALICE,
    profile: { id: ALICE.id, username: ALICE.username },
    preferences,
    groups: [],
    channels: {},
    groupMembers: {},
    dmChannels: [],
    messages: {},
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

function cssVar(page: Page, name: string): Promise<string> {
  return page.evaluate((n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(), name);
}

function storedPreferences(page: Page): Promise<Record<string, unknown>> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { preferences: Record<string, unknown> } }).__tauriMock.preferences,
  );
}

test("an accent picked on mobile (legacy accent_hex only) paints the desktop", async ({ page }) => {
  await boot(page, { accent_hex: "#bda3e0" });
  await expect.poll(() => cssVar(page, "--c-accent")).toBe("#bda3e0");
});

test("accent_color wins over the legacy key, and the background syncs too", async ({ page }) => {
  await boot(page, { accent_color: "#8ad6a7", accent_hex: "#bda3e0", background_color: "#090b0f" });
  await expect.poll(() => cssVar(page, "--c-accent")).toBe("#8ad6a7");
  expect(await cssVar(page, "--c-bg")).toBe("#090b0f");
});

test("a preset saves the shared keys plus the mirror older mobile builds read", async ({ page }) => {
  await boot(page, { skin: "refined" });
  await page.getByTestId("sidebar-row-preferences").click();
  await page.getByTestId("pref-accent-mint").click();
  await expect(page.getByTestId("pref-accent-mint")).toHaveAttribute("aria-pressed", "true");
  await expect.poll(async () => (await storedPreferences(page)).accent_color).toBe("#8ad6a7");
  const stored = await storedPreferences(page);
  expect(stored.accent_hex).toBe("#8ad6a7");
  expect(await cssVar(page, "--c-accent")).toBe("#8ad6a7");

  await page.getByTestId("pref-background-cool").click();
  await expect.poll(async () => (await storedPreferences(page)).background_color).toBe("#090b0f");
  expect(await cssVar(page, "--c-bg")).toBe("#090b0f");
});

test("no saved colours: the shared defaults, Modern on mobile's palette", async ({ page }) => {
  await boot(page, {});
  expect(await page.evaluate(() => document.documentElement.dataset.skin)).toBe("modern");
  expect(await cssVar(page, "--c-accent")).toBe("#fabf5a");
  expect(await cssVar(page, "--c-bg")).toBe("#0a0907");
  expect(await cssVar(page, "--c-text")).toBe("#f4e7d1");
  // One red in both skins.
  expect(await cssVar(page, "--c-danger")).toBe("#ff6b6b");
});
