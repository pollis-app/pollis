// Message-log render cost (#874): memoised rows must not re-render needlessly, yet must still update.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1XCB";

// Enough rows that a per-row regression is unmistakable rather than noise.
const ROW_COUNT = 12;
const OWN_ID = "01HQ7Z3K9M2P5R8T1V4W6Y10011";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function messages() {
  const out = [];
  for (let i = 0; i < ROW_COUNT; i++) {
    const own = i === ROW_COUNT - 1;
    out.push({
      id: own ? OWN_ID : `01HQ7Z3K9M2P5R8T1V4W6Y1${String(i).padStart(3, "0")}`,
      conversation_id: CHANNEL_ID,
      sender_id: own ? USER.id : "u-bob",
      content: own ? "my own newest message" : `message number ${i}`,
      // All on one day, so the rows render without a day boundary between them.
      sent_at: `2026-08-01T10:${String(i).padStart(2, "0")}:00.000Z`,
    });
  }
  return out;
}

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: USER.id,
        created_at: new Date().toISOString(),
      },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    dmChannels: [],
    messages: { [CHANNEL_ID]: messages() },
    bookmarks: [],
  };
}

async function boot(page: Page, skin: Skin) {
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
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function gotoChannel(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${OWN_ID}`)).toBeVisible();
}

/** Total render-function entries for a probed component since page load. */
async function renders(page: Page, name: string): Promise<number> {
  return await page.evaluate((probe) => {
    const w = window as unknown as { __pollisRenders?: Record<string, number> };
    return w.__pollisRenders?.[probe] ?? 0;
  }, name);
}

/** The probe must exist, or every "no new renders" assertion passes vacuously. */
async function assertProbeLive(page: Page) {
  expect(await renders(page, "MessageItem")).toBeGreaterThan(0);
  expect(await renders(page, "MessageList")).toBeGreaterThan(0);
}

/** Wait for the windowed log's post-layout re-render to settle before taking a baseline. */
async function settleRenders(page: Page) {
  let previous = -1;
  let stableFor = 0;
  await expect
    .poll(
      async () => {
        const current = await renders(page, "MessageList");
        stableFor = current === previous ? stableFor + 1 : 0;
        previous = current;
        // Three quiet polls in a row (~300ms): the list's post-mount scroll
        // settle lands a couple of hundred milliseconds after first paint, and
        // a single matching pair can fall on either side of it.
        return stableFor >= 3;
      },
      { intervals: [100, 100, 100, 100, 200, 200, 400] },
    )
    .toBe(true);
}

for (const skin of SKINS) {
  test.describe(`message log render cost — ${skin} skin`, () => {
    test("typing in the edit bar re-renders no message rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);

      const own = page.getByTestId(`message-${OWN_ID}`);
      await own.hover();
      await own.getByTestId("edit-button").click();
      const editInput = page.getByTestId("edit-message-bar-input");
      await expect(editInput).toBeVisible();

      // Baseline taken AFTER the bar is open: opening it is a legitimate state
      // change. What must cost nothing is the typing that follows.
      await settleRenders(page);
      const rowsBefore = await renders(page, "MessageItem");
      const listBefore = await renders(page, "MessageList");

      await editInput.click();
      await page.keyboard.type("edited text here", { delay: 10 });
      await expect(editInput).toHaveValue(/edited text here$/);

      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
      expect(await renders(page, "MessageList")).toBe(listBefore);
    });

    test("opening the reply bar re-renders no message rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);
      await settleRenders(page);

      const rowsBefore = await renders(page, "MessageItem");

      const own = page.getByTestId(`message-${OWN_ID}`);
      await own.hover();
      await own.getByTestId("reply-button").click();
      await expect(page.getByTestId("message-input")).toBeFocused();

      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
    });

    test("arrow-key log navigation re-renders no message rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);

      await page.getByTestId("message-input").click();
      await settleRenders(page);
      const rowsBefore = await renders(page, "MessageItem");

      // Rows style themselves via CSS :focus-within, so walking the log is
      // supposed to be free. Pin that.
      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OWN_ID}`)).toBeFocused();
      await page.keyboard.press("ArrowUp");
      await page.keyboard.press("ArrowUp");

      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
    });

    test("an open channel that nobody touches renders nothing", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);
      await settleRenders(page);

      // Not a settle: a fixed idle window in which nothing may render (an effect loop once re-rendered constantly).
      const before = await Promise.all(
        ["AppShell", "MainContent", "MessageList", "MessageItem", "ChatInput"].map((p) =>
          renders(page, p),
        ),
      );
      await page.waitForTimeout(1000);
      const after = await Promise.all(
        ["AppShell", "MainContent", "MessageList", "MessageItem", "ChatInput"].map((p) =>
          renders(page, p),
        ),
      );
      expect(after).toEqual(before);
    });

    test("typing in the composer re-renders only the composer", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);

      const input = page.getByTestId("message-input");
      await input.click();
      await settleRenders(page);
      const shellBefore = await renders(page, "AppShell");
      const contentBefore = await renders(page, "MainContent");
      const listBefore = await renders(page, "MessageList");
      const rowsBefore = await renders(page, "MessageItem");
      const composerBefore = await renders(page, "ChatInput");

      await page.keyboard.type("typing must stay local", { delay: 5 });
      await expect(input).toHaveText(/typing must stay local$/);

      // The composer rendered — otherwise the zeros below prove nothing.
      expect(await renders(page, "ChatInput")).toBeGreaterThan(composerBefore);
      expect(await renders(page, "AppShell")).toBe(shellBefore);
      expect(await renders(page, "MainContent")).toBe(contentBefore);
      expect(await renders(page, "MessageList")).toBe(listBefore);
      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
    });

    test("an AppShell re-render does not re-render the sidebar", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);
      await settleRenders(page);

      const shellBefore = await renders(page, "AppShell");
      const sidebarBefore = await renders(page, "Sidebar");

      // Cmd+K flips AppShell's own state; the memoised sidebar must absorb it.
      await page.keyboard.press(
        process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
      );
      await expect(page.getByTestId("search-panel")).toBeVisible();

      // The shell really did re-render — otherwise this proves nothing.
      expect(await renders(page, "AppShell")).toBeGreaterThan(shellBefore);
      expect(await renders(page, "Sidebar")).toBe(sidebarBefore);
    });

    test("flipping the skin restructures the rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // Refined rows are a two-column grid; the memoised row must not swallow a skin change.
      const rowDisplay = async () =>
        await page
          .getByTestId(`message-${OWN_ID}`)
          .evaluate((el) => getComputedStyle(el).display);

      expect(await rowDisplay()).toBe(skin === "refined" ? "grid" : "block");

      const other = skin === "terminal" ? "refined" : "terminal";
      // Navigate via the sidebar's testid (page.goto would re-serve the SPA root).
      await page.getByTestId("sidebar-row-preferences").click();
      await page.getByTestId(`pref-skin-${other}`).click();
      await expect(page.locator("html")).toHaveAttribute("data-skin", other);

      await gotoChannel(page);
      await expect(page.locator("html")).toHaveAttribute("data-skin", other);
      expect(await rowDisplay()).toBe(other === "refined" ? "grid" : "block");
    });

    test("editing a message updates that row's text", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      const own = page.getByTestId(`message-${OWN_ID}`);
      await expect(own).toContainText("my own newest message");

      await own.hover();
      await own.getByTestId("edit-button").click();
      const editInput = page.getByTestId("edit-message-bar-input");
      await expect(editInput).toBeVisible();
      await editInput.fill("the edited body");
      await page.keyboard.press("Enter");

      // The memoised row must reflect the new content.
      await expect(own).toContainText("the edited body");
    });
  });
}
