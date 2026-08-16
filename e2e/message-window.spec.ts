/*
 * The windowed message log (#874) — browser-level e2e.
 *
 * Phase 2 made RENDER cost independent of history length; this file covers the
 * phase that made LAYOUT and PAINT independent of it too, by keeping only a
 * bounded window of rows in the document.
 *
 * The interesting tests here are not "the window is small" — that one is easy
 * and would pass on a broken build that simply lost half the log. They are the
 * three subsystems that locate a row with `querySelector` and therefore break
 * the moment a row can be absent: permalink jumps, arrow-key focus projection,
 * and reply-quote scrolling. Each is deliberately pointed at a target far
 * outside the initial window, because a target inside it proves nothing.
 *
 * Runs in both skins: the two have different row shapes, different heights and
 * different grouping rules, and the window measures real rows.
 *
 *   pnpm --filter @pollis/e2e exec playwright test message-window
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2XCB";

/**
 * Long enough that the window has to be a real window. At ~2rem a row the
 * viewport holds roughly fifteen, so 400 is more than an order of magnitude
 * past anything an overscan could hide.
 */
const TOTAL = 400;
/** Messages on the first of the two seeded days. Kept a multiple of the
 *  sender run length so a day boundary never lands mid-group. */
const FIRST_DAY = 200;
/** Consecutive messages from one author, i.e. one refined sender group. */
const RUN = 5;

const idAt = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y2${String(i).padStart(3, "0")}`;

const NEWEST = idAt(TOTAL - 1);
/** Deep in the history, nowhere near the initial window. */
const ANCIENT = idAt(3);
/** The message the newest one quotes — also far outside the window. */
const QUOTED = idAt(7);

function messages() {
  const out = [];
  for (let i = 0; i < TOTAL; i++) {
    const day = i < FIRST_DAY ? "01" : "02";
    const minute = i % FIRST_DAY;
    out.push({
      id: idAt(i),
      conversation_id: CHANNEL_ID,
      // Runs of RUN from one author, so `isGroupStart` is exactly `i % RUN`.
      sender_id: Math.floor(i / RUN) % 2 === 0 ? "u-bob" : USER.id,
      content: `message number ${i}`,
      // One per minute inside a day: never the >5min gap that would start a
      // new sender group on its own.
      sent_at: `2026-08-${day}T${String(10 + Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}:00.000Z`,
      ...(i === TOTAL - 1 ? { reply_to_id: QUOTED } : {}),
    });
  }
  return out;
}

type Skin = "terminal" | "refined";
const SKINS: Skin[] = ["terminal", "refined"];

function preloadState(skin: Skin, opts: { paginate?: boolean } = {}) {
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
    ...opts,
  };
}

async function boot(page: Page, skin: Skin, opts: { paginate?: boolean } = {}) {
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
  }, preloadState(skin, opts));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openCommandPalette(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

async function gotoChannel(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  // The log opens at its newest message, as a chat log must.
  await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();
}

/**
 * Message rows currently in the document, counted by the row's own test id so
 * the count means the same thing with and without the window — which is what
 * makes "reverting the window fails this" a real check rather than a check
 * that a wrapper element is missing.
 */
function renderedRows(page: Page) {
  return page.locator('[data-testid^="message-01"]');
}

/** The windowed row wrappers, which carry the timeline index. */
function windowedRows(page: Page) {
  return page.locator('[data-testid="message-window"] > [data-index]');
}

/** Every element inside the scroll container — the honest cost metric. */
async function domNodesInLog(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      document
        .querySelector('[data-testid="message-list"]')
        ?.querySelectorAll("*").length ?? 0,
  );
}

async function scrollLogTo(page: Page, top: number) {
  await page.evaluate((to) => {
    const el = document.querySelector('[data-testid="message-list"]');
    if (el) {
      el.scrollTop = to === -1 ? el.scrollHeight : to;
    }
  }, top);
}

/**
 * Record every element that receives the jump-highlight class. Same reason as
 * `bookmarks.spec.ts`: asserting the class directly races its own removal.
 */
async function watchForFlash(page: Page) {
  await page.evaluate(() => {
    const w = window as unknown as { __flashed: string[] };
    w.__flashed = [];
    new MutationObserver((records) => {
      for (const r of records) {
        const el = r.target as HTMLElement;
        if (el.classList?.contains("pollis-message-flash")) {
          const id = el.getAttribute("data-testid");
          if (id && !w.__flashed.includes(id)) {
            w.__flashed.push(id);
          }
        }
      }
    }).observe(document.body, {
      subtree: true,
      attributes: true,
      attributeFilter: ["class"],
    });
  });
}

async function flashedTestIds(page: Page): Promise<string[]> {
  return page.evaluate(
    () => (window as unknown as { __flashed: string[] }).__flashed ?? [],
  );
}

for (const skin of SKINS) {
  test.describe(`windowed message log — ${skin} skin`, () => {
    test("a 400-message log keeps a bounded number of rows in the DOM", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      const rendered = await renderedRows(page).count();
      // A viewport of ~15 rows plus 10 rows of overscan on each side. The
      // ceiling is loose on purpose — this pins the ORDER, which is what
      // regressing back to "render everything" would break, and 400 rows
      // would sail past it.
      expect(rendered).toBeGreaterThan(0);
      expect(rendered).toBeLessThan(80);

      // The array behind the window is still the whole history: the scroll
      // height reflects 400 rows even though 400 rows are not present.
      const totalHeight = await page
        .getByTestId("message-window")
        .evaluate((el) => el.getBoundingClientRect().height);
      const viewport = await page
        .getByTestId("message-list")
        .evaluate((el) => el.clientHeight);
      expect(totalHeight).toBeGreaterThan(viewport * 5);
    });

    test("the whole history is still reachable by scrolling", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // The oldest message is not in the document to begin with — otherwise
      // the scroll below would prove nothing.
      await expect(page.getByTestId(`message-${idAt(0)}`)).toHaveCount(0);

      await scrollLogTo(page, 0);
      await expect(page.getByTestId(`message-${idAt(0)}`)).toBeVisible();
      await expect(page.getByTestId(`message-${idAt(0)}`)).toContainText(
        "message number 0",
      );
      // Still bounded up here — the window moved, it did not grow.
      expect(await renderedRows(page).count()).toBeLessThan(80);

      // And back down again, without the newest message having gone anywhere.
      await scrollLogTo(page, -1);
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();
    });

    test("a permalink jumps to a message far outside the window and flashes it", async ({
      page,
    }) => {
      await boot(page, skin);
      await watchForFlash(page);
      await gotoChannel(page);

      // Precondition: the target genuinely is not rendered.
      await expect(page.getByTestId(`message-${ANCIENT}`)).toHaveCount(0);

      await openCommandPalette(page);
      await page
        .getByTestId("search-panel-input")
        .fill(`pollis://m/${CHANNEL_ID}/${ANCIENT}`);

      const target = page.getByTestId(`message-${ANCIENT}`);
      await expect(target).toBeVisible();
      await expect(target).toContainText("message number 3");
      await expect
        .poll(() => flashedTestIds(page))
        .toContain(`message-${ANCIENT}`);
      expect(await renderedRows(page).count()).toBeLessThan(80);
    });

    test("arrow-key navigation walks into rows that were outside the window", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      await page.getByTestId("message-input").click();
      await expect(page.getByTestId("message-input")).toBeFocused();

      // 60 rows up from the newest — well past the initial window.
      const STEPS = 60;
      const destination = idAt(TOTAL - 1 - STEPS);
      await expect(page.getByTestId(`message-${destination}`)).toHaveCount(0);

      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeFocused();
      for (let step = 1; step <= STEPS; step++) {
        await page.keyboard.press("ArrowUp");
        // Asserting each step keeps the composer from re-claiming ArrowUp
        // while a reveal is still in flight, and localises any failure.
        await expect(
          page.getByTestId(`message-${idAt(TOTAL - 1 - step)}`),
        ).toBeFocused();
      }

      await expect(page.getByTestId(`message-${destination}`)).toBeVisible();
      // The row's action bar is reachable, which means the nav machine could
      // read it out of the DOM after the window moved.
      await page.keyboard.press("ArrowRight");
      await expect(
        page.getByTestId(`message-${destination}`).getByTestId("reply-button"),
      ).toBeFocused();

      // And the window is still a window.
      expect(await renderedRows(page).count()).toBeLessThan(80);
    });

    test("a reply quote scrolls to a parent outside the window", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      await expect(page.getByTestId(`message-${QUOTED}`)).toHaveCount(0);

      const newest = page.getByTestId(`message-${NEWEST}`);
      await expect(newest).toBeVisible();
      await newest.getByTestId(`reply-preview-${QUOTED}`).click();

      const parent = page.getByTestId(`message-${QUOTED}`);
      await expect(parent).toBeVisible();
      await expect(parent).toContainText("message number 7");
    });

    test("day dividers come from the whole timeline, not the window", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // Bottom of the log: deep inside the second day, so no boundary is on
      // screen. This is the assertion that a window edge does not FABRICATE a
      // divider — computing the boundary against the previous rendered row
      // instead of the previous timeline entry would put one on every window.
      await expect(page.getByTestId("day-divider")).toHaveCount(0);

      // The first message of the log always opens a day.
      await scrollLogTo(page, 0);
      await expect(page.getByTestId(`message-${idAt(0)}`)).toBeVisible();
      const dayOne = await page.getByTestId("day-divider").allInnerTexts();
      expect(dayOne).toHaveLength(1);

      // And the real boundary, mid-history, is still there once its row is —
      // a different day, so a different label.
      await openCommandPalette(page);
      await page
        .getByTestId("search-panel-input")
        .fill(`pollis://m/${CHANNEL_ID}/${idAt(FIRST_DAY)}`);
      await expect(page.getByTestId(`message-${idAt(FIRST_DAY)}`)).toBeVisible();
      const dayTwo = await page.getByTestId("day-divider").allInnerTexts();
      expect(dayTwo).toHaveLength(1);
      expect(dayTwo[0]).not.toEqual(dayOne[0]);
    });

    test("older pages prepend without moving the message being read", async ({
      page,
    }) => {
      await boot(page, skin, { paginate: true });
      await openCommandPalette(page);
      await page.getByTestId("search-panel-input").fill("general");
      await page.keyboard.press("Enter");

      // One page only: the log opens on the newest 50 and nothing older.
      const firstPageOldest = page.getByTestId(`message-${idAt(TOTAL - 50)}`);
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();
      await scrollLogTo(page, 0);
      await expect(firstPageOldest).toBeVisible();

      // Scrolling to the top triggers the load-more; the row that was under
      // the reader must not be dragged away by 50 rows of prepended history.
      const before = await firstPageOldest.boundingBox();
      await expect(page.getByTestId(`message-${idAt(TOTAL - 51)}`)).toHaveCount(
        1,
      );
      const after = await firstPageOldest.boundingBox();
      expect(before).not.toBeNull();
      expect(after).not.toBeNull();
      // One row of tolerance: the prepended rows are measured as they mount,
      // so the anchor settles rather than snapping.
      expect(Math.abs((after?.y ?? 0) - (before?.y ?? 0))).toBeLessThan(60);
    });
  });
}

test.describe("windowed message log — refined grouping", () => {
  test("sender grouping survives the window", async ({ page }) => {
    await boot(page, "refined");
    await gotoChannel(page);

    // Seeded in runs of five, so the last five rows are one sender group:
    // index 395 opens it and carries the header, 396..399 are follow-ups and
    // carry none. Grouping is computed from the previous TIMELINE entry, so
    // this is also what would break if it were computed from the window.
    await expect(
      page.getByTestId(`message-${idAt(395)}`).getByTestId("message-author"),
    ).toHaveCount(1);
    for (const i of [396, 397, 398, 399]) {
      await expect(
        page.getByTestId(`message-${idAt(i)}`).getByTestId("message-author"),
      ).toHaveCount(0);
    }

    // The row at the very top of the window is the one a window-local
    // computation would wrongly promote to a group start, since it has no
    // rendered predecessor. Whether it carries a header must depend only on
    // its index in the timeline.
    const topIndex = Number(
      await windowedRows(page).first().getAttribute("data-index"),
    );
    expect(Number.isFinite(topIndex)).toBe(true);
    const topRowHasHeader = await page
      .locator(`[data-index="${topIndex}"] [data-testid="message-author"]`)
      .count();
    expect(topRowHasHeader).toBe(topIndex % RUN === 0 ? 1 : 0);
  });

  test("a row scrolled back into the window is grouped the same way", async ({
    page,
  }) => {
    await boot(page, "refined");
    await gotoChannel(page);

    // Index 200 opens the second day AND a sender run, so it must carry a
    // header; 201 must not. Neither is rendered until we go there, which is
    // the point: the window mounts them fresh and grouping has to survive it.
    await expect(page.getByTestId(`message-${idAt(200)}`)).toHaveCount(0);
    await openCommandPalette(page);
    await page
      .getByTestId("search-panel-input")
      .fill(`pollis://m/${CHANNEL_ID}/${idAt(200)}`);

    await expect(page.getByTestId(`message-${idAt(200)}`)).toBeVisible();
    await expect(
      page.getByTestId(`message-${idAt(200)}`).getByTestId("message-author"),
    ).toHaveCount(1);
    await expect(
      page.getByTestId(`message-${idAt(201)}`).getByTestId("message-author"),
    ).toHaveCount(0);
  });
});

test.describe("windowed message log — cost", () => {
  test("DOM size does not scale with history length", async ({ page }) => {
    await boot(page, "terminal");
    await gotoChannel(page);

    // The number that #874 is actually about. Unwindowed, 400 terminal rows
    // are roughly 5,000 elements; the window holds a small constant.
    const nodes = await domNodesInLog(page);
    expect(nodes).toBeGreaterThan(0);
    expect(nodes).toBeLessThan(1500);
  });
});
