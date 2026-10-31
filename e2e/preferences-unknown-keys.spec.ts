// A desktop preferences save keeps the keys only mobile writes (#1650).

import { test, expect, type Page } from "./lib/zero-loss";

const ALICE = { id: "u-alice", email: "alice@example.com", username: "alice" };

const MOBILE_KEYS = {
  accent_hex: "#5ab0fa",
  mobile_behavior: { haptics: false, send_on_enter: true },
};

async function boot(page: Page) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
      plugins: {},
      transformCallback: () => 0,
      convertFileSrc: (p: string) => p,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, {
    session: ALICE,
    profile: { id: ALICE.id, username: ALICE.username },
    preferences: JSON.stringify({ skin: "terminal", font_size: "16", ...MOBILE_KEYS }),
    groups: [],
    channels: {},
    groupMembers: {},
    dmChannels: [],
    messages: {},
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

function storedPreferences(page: Page): Promise<Record<string, unknown>> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { preferences: Record<string, unknown> } }).__tauriMock.preferences,
  );
}

test("a Preferences page save keeps mobile's keys and drops only legacy font_size", async ({ page }) => {
  await boot(page);
  await page.getByTestId("sidebar-row-preferences").click();
  await expect(page.getByTestId("preferences-page")).toBeVisible();

  await page.getByTestId("pref-skin-refined").click();

  await expect.poll(async () => (await storedPreferences(page)).skin).toBe("refined");
  const stored = await storedPreferences(page);
  expect(stored.accent_hex).toBe(MOBILE_KEYS.accent_hex);
  expect(stored.mobile_behavior).toEqual(MOBILE_KEYS.mobile_behavior);
  // Deleted on purpose: font size is device-local now.
  expect("font_size" in stored).toBe(false);

  // The save itself carries them, so a replacing (pre-#1650) DS keeps them too.
  const sent = await page.evaluate(() => {
    const args = (window as unknown as { __tauriLastArgs: Record<string, { preferencesJson: string }> })
      .__tauriLastArgs.save_preferences;
    return JSON.parse(args.preferencesJson) as Record<string, unknown>;
  });
  expect(sent.accent_hex).toBe(MOBILE_KEYS.accent_hex);
  expect(sent.mobile_behavior).toEqual(MOBILE_KEYS.mobile_behavior);
  expect(sent.font_size).toBeNull();
});
