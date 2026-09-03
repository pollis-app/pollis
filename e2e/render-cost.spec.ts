/*
 * Message-log render cost — browser-level e2e (#874).
 *
 * These are REGRESSION GUARDS, not proof that the app is fast. They pin one
 * specific property that is easy to lose by accident and invisible when lost:
 * a component that only shows message content must not re-render because
 * something unrelated changed. The evidence is a render counter
 * (`frontend/src/utils/renderProbe.ts`) incremented at the top of the
 * component body, so a `React.memo` bail-out shows up as "no new renders" —
 * which is exactly the thing being asserted.
 *
 * The second half is the other side of the same coin and matters MORE: a memo
 * that never updates is a stale UI, which is worse than a slow one. So every
 * "must not re-render" case is paired with a "must still re-render" case.
 *
 * Counts are relative on purpose. The e2e build runs under StrictMode, which
 * double-invokes render in dev, so absolute numbers are 2x and must never be
 * hard-coded.
 *
 *   pnpm --filter @pollis/e2e exec playwright test render-cost
 */

import { test, expect, type Page } from "@playwright/test";

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
      // All on one day so the day-divider count is deterministic.
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

/**
 * The probe must exist at all — otherwise every "no new renders" assertion
 * below passes vacuously against a counter that is stuck on zero, and the
 * whole file becomes a guard that guards nothing.
 */
async function assertProbeLive(page: Page) {
  expect(await renders(page, "MessageItem")).toBeGreaterThan(0);
  expect(await renders(page, "MessageList")).toBeGreaterThan(0);
}

/**
 * Wait until the log has stopped re-rendering of its own accord.
 *
 * Since the log is windowed (#874) it renders once more after any layout
 * change, when the virtualizer measures the rows it has just mounted. That
 * settle is legitimate and is not what these tests are about — they are about
 * what the NEXT interaction costs — so the baseline must be taken after it,
 * not in the middle of it. Polling for a stable counter rather than sleeping
 * keeps the assertions below exact instead of merely likely.
 */
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

      // Open the edit bar on the viewer's own message.
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

      // Not a settle: a fixed window with the app left alone. The ChannelPage
      // used to depend on a `useMutation` result object — new every render —
      // from an effect whose cleanup nulled the selected channel, so an open
      // channel re-rendered the whole tree and fired an IPC hundreds of times a
      // second for as long as it stayed open. Every keystroke in the composer
      // competed with that, which is what "typing feels slow" was. Nothing in
      // this window is allowed to render, at any level of the tree.
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

      // Cmd+K flips `isSearchOpen`, which is AppShell's own state. The sidebar
      // (group tree, DM list, unread badges) has nothing to do with the search
      // panel, and it is memoised — so it must absorb this at its own boundary
      // rather than re-rendering because the shell handed it a fresh callback.
      await page.keyboard.press(
        process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
      );
      await expect(page.getByTestId("search-panel")).toBeVisible();

      // The shell really did re-render — otherwise this proves nothing.
      expect(await renders(page, "AppShell")).toBeGreaterThan(shellBefore);
      expect(await renders(page, "Sidebar")).toBe(sidebarBefore);
    });

    // ── The other half: memoised rows MUST still update ──────────────────

    test("flipping the skin restructures the rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // The row's own structure, not just its colors: refined lays each row out
      // as a two-column CSS grid (avatar gutter + content), terminal does not.
      // `skin` used to be a per-row subscription; it now arrives as a prop on a
      // MEMOISED component, so this is the assertion that the memo cannot
      // swallow a skin change and leave the log rendering the old structure.
      const rowDisplay = async () =>
        await page
          .getByTestId(`message-${OWN_ID}`)
          .evaluate((el) => getComputedStyle(el).display);

      expect(await rowDisplay()).toBe(skin === "refined" ? "grid" : "block");

      const other = skin === "terminal" ? "refined" : "terminal";
      // In-app navigation via the sidebar — a bare `page.goto` on a
      // client-side route just re-serves the SPA shell at its root. Located by
      // `data-testid`, not by the row's translated label (#932).
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

    test("day dividers survive the memoised row path", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // One divider: every seeded message shares a single local day.
      await expect(page.getByTestId("day-divider")).toHaveCount(1);
    });
  });
}
