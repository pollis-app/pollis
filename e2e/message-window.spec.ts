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

/** A second channel with a history of its own, for the tests that need to
 *  leave a conversation and come back to it (#927). */
const OTHER_CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2OCB";
const otherIdAt = (i: number) =>
  `01HQ7Z3K9M2P5R8T1V4W6Y3${String(i).padStart(3, "0")}`;

function messagesFor(
  conversationId: string,
  id: (i: number) => string,
  withReply: boolean,
) {
  const out = [];
  for (let i = 0; i < TOTAL; i++) {
    const day = i < FIRST_DAY ? "01" : "02";
    const minute = i % FIRST_DAY;
    out.push({
      id: id(i),
      conversation_id: conversationId,
      // Runs of RUN from one author, so `isGroupStart` is exactly `i % RUN`.
      sender_id: Math.floor(i / RUN) % 2 === 0 ? "u-bob" : USER.id,
      content: `message number ${i}`,
      // One per minute inside a day: never the >5min gap that would start a
      // new sender group on its own.
      sent_at: `2026-08-${day}T${String(10 + Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}:00.000Z`,
      ...(withReply && i === TOTAL - 1 ? { reply_to_id: QUOTED } : {}),
    });
  }
  return out;
}

function messages() {
  return messagesFor(CHANNEL_ID, idAt, true);
}

type Skin = "terminal" | "refined";
const SKINS: Skin[] = ["terminal", "refined"];

type BootOptions = {
  paginate?: boolean;
  /** Seed a second channel, "archive", with a history of its own. */
  secondChannel?: boolean;
};

function preloadState(skin: Skin, opts: BootOptions = {}) {
  const { secondChannel, ...preloadOpts } = opts;
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
      [GROUP_ID]: [
        { id: CHANNEL_ID, group_id: GROUP_ID, name: "general" },
        ...(secondChannel
          ? [{ id: OTHER_CHANNEL_ID, group_id: GROUP_ID, name: "archive" }]
          : []),
      ],
    },
    dmChannels: [],
    messages: {
      [CHANNEL_ID]: messages(),
      ...(secondChannel
        ? { [OTHER_CHANNEL_ID]: messagesFor(OTHER_CHANNEL_ID, otherIdAt, false) }
        : {}),
    },
    bookmarks: [],
    ...preloadOpts,
  };
}

async function boot(page: Page, skin: Skin, opts: BootOptions = {}) {
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

/** Open a channel by name and wait for it to land on its newest message. */
async function openChannel(page: Page, name: string, newestId: string) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill(name);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${newestId}`)).toBeVisible();
}

/**
 * Scroll back through the log the way a reader does — a real wheel over the
 * message list, not an assignment to `scrollTop`, because the windowed log
 * re-anchors itself and a synthetic offset is not what the browser hands a
 * scrolling user.
 *
 * Stops as soon as `untilId` is in the document, so a passing run costs only
 * the scrolling it needed. Bounded: `MAX_WHEELS` is several times the ~21
 * steps a 400-message history takes at 50 messages a page.
 */
const MAX_WHEELS = 90;
async function wheelBackTo(page: Page, untilId: string): Promise<boolean> {
  await page.getByTestId("message-list").hover();
  for (let i = 0; i < MAX_WHEELS; i++) {
    if ((await page.getByTestId(`message-${untilId}`).count()) > 0) {
      return true;
    }
    await page.mouse.wheel(0, -600);
    await page.waitForTimeout(80);
  }
  return (await page.getByTestId(`message-${untilId}`).count()) > 0;
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

    test("scrolling back reaches the oldest message, not just the newest page (guard)", async ({
      page,
    }) => {
      await boot(page, skin, { paginate: true });
      await gotoChannel(page);

      // 400 messages, 50 to a page: getting to the first one means seven
      // pages after the one the log opened with.
      const oldest = page.getByTestId(`message-${idAt(0)}`);
      await expect(oldest).toHaveCount(0);

      expect(await wheelBackTo(page, idAt(0))).toBe(true);
      await expect(oldest).toBeVisible();
      await expect(oldest).toContainText("message number 0");

      // Every page arrived, and none of them arrived twice.
      const distinct = await page.evaluate(
        () =>
          new Set(
            Array.from(
              document.querySelectorAll('[data-testid^="message-01"]'),
            ).map((el) => el.getAttribute("data-testid")),
          ).size,
      );
      expect(distinct).toBe(await renderedRows(page).count());
      // And it is still a window, not 400 rows in the document.
      expect(await renderedRows(page).count()).toBeLessThan(80);
    });

    test("a conversation reopened from cache can still reach its oldest message", async ({
      page,
    }) => {
      await boot(page, skin, { paginate: true, secondChannel: true });

      // Visit "archive" once so its first page is in the query cache, then
      // read back through "general" so there ARE older pages held for some
      // other conversation.
      await openChannel(page, "archive", otherIdAt(TOTAL - 1));
      await openChannel(page, "general", NEWEST);
      expect(await wheelBackTo(page, idAt(TOTAL - 60))).toBe(true);

      // Back to "archive", now served from cache. Its cursor used to be
      // seeded by an effect that read the pages belonging to the conversation
      // just left — non-empty, so it declined to seed anything, and no
      // dependency would ever change to make it try again. The channel was
      // pinned to its newest 50 messages for as long as the cache held it,
      // with the rest sitting on disk unreachable (#927).
      await openChannel(page, "archive", otherIdAt(TOTAL - 1));
      expect(await wheelBackTo(page, otherIdAt(0))).toBe(true);
      await expect(page.getByTestId(`message-${otherIdAt(0)}`)).toBeVisible();
      await expect(
        page.getByTestId(`message-${otherIdAt(0)}`),
      ).toContainText("message number 0");
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

/*
 * The load-more seam (#934).
 *
 * `MainContent` used to clear `loadingMore` in the same `finally` that
 * prepended the page. React batches every setState from one async
 * continuation, so the rows and `loadingMore === false` landed in a single
 * commit and there was no committed — or painted — state in which the fetched
 * rows existed while the log still said it was fetching.
 *
 * That state is the seam anything reacting between "the page arrived" and "the
 * load is over" needs: a scroll anchor, a spinner that must not flash, a
 * timing measurement. Asserted through a MutationObserver rather than by
 * polling, because polling cannot see a state that lasts one commit — and
 * because the observer callback fires once per DOM mutation BATCH, which is
 * exactly the granularity the claim is about: if both changes were still in
 * one commit, no batch would ever contain new rows alongside the loading line.
 */
test.describe("load-more leaves a frame with the new rows and the flag still set", () => {
  /** The newest row of the SECOND page — i.e. one that only a load-more can
   *  put in the document, and near enough the window edge to be rendered. */
  const FIRST_OLDER_ROW = idAt(TOTAL - 51);

  for (const skin of SKINS) {
    test(`${skin}: a prepend batch still carries the loading line`, async ({
      page,
    }) => {
      await boot(page, skin, { paginate: true });
      await openCommandPalette(page);
      await page.getByTestId("search-panel-input").fill("general");
      await page.keyboard.press("Enter");
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();

      // Nothing older than the first page is in the document yet.
      await expect(page.getByTestId(`message-${FIRST_OLDER_ROW}`)).toHaveCount(0);

      await page.evaluate((olderRow) => {
        const w = window as unknown as {
          __seam: { withLoading: number; withoutLoading: number };
        };
        w.__seam = { withLoading: 0, withoutLoading: 0 };
        const loadingLine = () =>
          !!document.querySelector('[data-testid="message-loading-older"]');
        const olderRowPresent = () =>
          !!document.querySelector(`[data-testid="message-${olderRow}"]`);
        // Every mutation batch in which the older row is present gets
        // classified by whether the log still calls itself busy.
        new MutationObserver(() => {
          if (!olderRowPresent()) {
            return;
          }
          if (loadingLine()) {
            w.__seam.withLoading++;
          } else {
            w.__seam.withoutLoading++;
          }
        }).observe(document.body, { childList: true, subtree: true });
      }, FIRST_OLDER_ROW);

      // Scrolling to the top is what asks for the next page.
      await scrollLogTo(page, 0);
      await expect(page.getByTestId(`message-${FIRST_OLDER_ROW}`)).toHaveCount(1);
      // …and the load does finish: the flag is not simply stuck on.
      await expect(page.getByTestId("message-loading-older")).toHaveCount(0);

      const seam = await page.evaluate(
        () =>
          (window as unknown as {
            __seam: { withLoading: number; withoutLoading: number };
          }).__seam,
      );

      expect(
        seam.withLoading,
        "no batch contained the prepended rows while the log still reported " +
          "fetching — the prepend and the flag clear are back in one commit (#934)",
      ).toBeGreaterThan(0);
      // And the end state really is reached, so the assertion above is not
      // passing on a log that never stopped loading.
      expect(seam.withoutLoading).toBeGreaterThan(0);
    });
  }
});
