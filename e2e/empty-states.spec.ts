// One empty-state treatment (#1969), both skins: an empty list says so in one centred sentence
// through `EmptyState`, never a left-aligned line of body text, and its action sits under it.

import { test, expect, type Locator, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

async function boot(page: Page, skin: Skin) {
  const state = {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      { id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    dmChannels: [],
    messages: {},
    bookmarks: [],
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

/** Open a page through Cmd+K, as a person would. */
async function openPage(page: Page, name: string, pageTestId: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(name);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(pageTestId)).toBeVisible();
}

/** The sentence is centred in its container, in the skin's muted ink. */
async function expectCentred(page: Page, container: Locator, text: string) {
  const empty = container.locator("[data-empty-state]");
  await expect(empty).toHaveCount(1);
  await expect(empty).toBeVisible();
  const sentence = empty.locator("p").first();
  await expect(sentence).toHaveText(text);

  const box = await container.boundingBox();
  const line = await sentence.boundingBox();
  expect(box && line).toBeTruthy();
  const boxMid = box!.x + box!.width / 2;
  const lineMid = line!.x + line!.width / 2;
  expect(Math.abs(boxMid - lineMid)).toBeLessThan(4);
  // Centred in the pane, not pinned to its top edge the way the old body-text line was.
  const boxCentre = box!.y + box!.height / 2;
  const lineCentre = line!.y + line!.height / 2;
  expect(Math.abs(boxCentre - lineCentre)).toBeLessThan(8);

  const muted = await page.evaluate(() => {
    const probe = document.createElement("span");
    probe.className = "text-muted";
    document.body.appendChild(probe);
    const colour = getComputedStyle(probe).color;
    probe.remove();
    return colour;
  });
  await expect(sentence).toHaveCSS("color", muted);
}

for (const skin of SKINS) {
  test.describe(`empty states (${skin})`, () => {
    test("Blocked users with nobody blocked", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "Blocked Users", "blocked-page");
      await expectCentred(page, page.getByTestId("blocked-page"), "No blocked users.");
      await page.screenshot({ path: `artifacts/empty-blocked-${skin}.png` });
    });

    test("Saved with nothing saved", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "Saved", "saved-page");
      await expectCentred(
        page,
        page.getByTestId("saved-page"),
        "No saved messages. Use the bookmark action on a message to save it.",
      );
      await page.screenshot({ path: `artifacts/empty-saved-${skin}.png` });
    });

    test("DM requests with none waiting", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "DM Requests", "requests-page");
      await expectCentred(page, page.getByTestId("requests-page"), "No pending requests.");
      await page.screenshot({ path: `artifacts/empty-dm-requests-${skin}.png` });
    });
  });
}
