/*
 * Jump-to-message must not leave a gap behind it (#1039) — browser-level e2e.
 *
 * A permalink or search hit pointing deep into a long conversation fetches
 * the page AROUND the target (#850). Before #1039 that page was simply merged
 * with the live newest page, and the log rendered the two as if they were
 * contiguous — the reader could scroll from message 150 straight into message
 * 350 with nothing to say two hundred messages were missing. The log now
 * opens an anchored window around the target instead and pages it forward
 * until it provably rejoins the live tail.
 *
 * The check is therefore a walk: jump far back, wheel forward to the newest
 * message, and demand that every message id in between was rendered on the
 * way. `paginate: true` is what makes the mock answer the around-read with a
 * real bounded page, so the gap exists to be crossed.
 *
 * Runs in both skins: row heights differ, and the load-newer trigger is a
 * distance from the bottom of a real scroll container.
 *
 *   pnpm --filter @pollis/e2e exec playwright test message-jump-gap
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2XCB";

/** Eight pages of fifty: the jump target below sits three pages clear of
 *  the live one on either side of its own around-page. */
const TOTAL = 400;
/** Messages on the first of the two seeded days. */
const FIRST_DAY = 200;
/** Consecutive messages from one author, i.e. one refined sender group. */
const RUN = 5;
/** What the mock and the renderer agree a page is. */
const PAGE = 50;

const idAt = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y2${String(i).padStart(3, "0")}`;

const NEWEST = idAt(TOTAL - 1);
/** The jump target. Its around-page is 50..150; the live page is 350..399;
 *  everything from 151 to 349 is the gap. */
const TARGET = 100;

function messages() {
  const out = [];
  for (let i = 0; i < TOTAL; i++) {
    const day = i < FIRST_DAY ? "01" : "02";
    const minute = i % FIRST_DAY;
    out.push({
      id: idAt(i),
      conversation_id: CHANNEL_ID,
      sender_id: Math.floor(i / RUN) % 2 === 0 ? "u-bob" : USER.id,
      content: `message number ${i}`,
      sent_at: `2026-08-${day}T${String(10 + Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}:00.000Z`,
    });
  }
  return out;
}

type Skin = "terminal" | "refined";
const SKINS: Skin[] = ["terminal", "refined"];

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      { id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    dmChannels: [],
    messages: { [CHANNEL_ID]: messages() },
    bookmarks: [],
    paginate: true,
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

async function openCommandPalette(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

async function gotoChannel(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();
}

/** Resolve a permalink to a message, which is what a search hit does too. */
async function jumpTo(page: Page, index: number) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill(`pollis://m/${CHANNEL_ID}/${idAt(index)}`);
  const target = page.getByTestId(`message-${idAt(index)}`);
  await expect(target).toBeVisible();
  await expect(target).toContainText(`message number ${index}`);
}

/**
 * The other door into the same jump: a search hit navigates to the channel
 * with `?message=` on the URL. This is the path that already fetched the
 * around-page before #1039 — and then merged it straight into the live one.
 */
async function jumpViaSearchHit(page: Page, index: number) {
  await openCommandPalette(page);
  const palette = page.getByTestId("search-panel-input");
  await palette.fill("Search Messages");
  await expect(palette).toHaveValue("Search Messages");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("search-view")).toBeVisible();
  await page.getByTestId("search-input").fill(`number ${index}`);
  const hit = page.getByTestId("search-result-item").first();
  await expect(hit).toContainText(`message number ${index}`);
  await hit.click();
  const target = page.getByTestId(`message-${idAt(index)}`);
  await expect(target).toBeVisible();
  await expect(target).toContainText(`message number ${index}`);
}

/** Indices of the seeded messages currently rendered, in document order. */
async function renderedIndices(page: Page): Promise<number[]> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll('[data-testid^="message-01"]')).map((el) =>
      Number((el.getAttribute("data-testid") ?? "").slice(-3)),
    ),
  );
}

/**
 * Wheel forward through the log the way a reader does, remembering every
 * message that was rendered along the way, until `untilId` is in the
 * document. Steps are shorter than the window's overscan, so a message the
 * log actually holds cannot slip past unrendered — which is what lets the
 * caller treat "never rendered" as "never loaded".
 *
 * Bounded: 300 messages at a dozen rows a step is ~25 steps plus the pages
 * they ask for; 200 is several times that.
 */
const MAX_WHEELS = 200;
async function wheelForwardTo(page: Page, untilId: string): Promise<Set<number>> {
  const seen = new Set<number>();
  await page.getByTestId("message-list").hover();
  for (let i = 0; i < MAX_WHEELS; i++) {
    for (const index of await renderedIndices(page)) {
      seen.add(index);
    }
    if ((await page.getByTestId(`message-${untilId}`).count()) > 0) {
      return seen;
    }
    await page.mouse.wheel(0, 400);
    await page.waitForTimeout(80);
  }
  return seen;
}

/**
 * The row's box once the log has stopped moving. The jump reveals its target
 * with a smooth scroll, so a box read straight after "visible" is a box
 * mid-animation.
 */
async function settledBox(page: Page, testId: string) {
  const row = page.getByTestId(testId);
  let last = await row.boundingBox();
  for (let i = 0; i < 50; i++) {
    await page.waitForTimeout(100);
    const next = await row.boundingBox();
    if (last && next && Math.abs(next.y - last.y) < 0.5) {
      return next;
    }
    last = next;
  }
  return last;
}

/** Push a message into the mock's store and announce it, as the DS would. */
async function arrive(page: Page, index: number) {
  await page.evaluate(
    ({ channelId, id, index, senderId }) => {
      const w = window as unknown as {
        __tauriMock: { messages: Record<string, unknown[]> };
        __emitRealtimeEvent: (event: unknown) => number;
      };
      w.__tauriMock.messages[channelId].push({
        id,
        conversation_id: channelId,
        sender_id: senderId,
        content: `message number ${index}`,
        sent_at: new Date().toISOString(),
      });
      w.__emitRealtimeEvent({
        type: "new_message",
        channel_id: channelId,
        conversation_id: null,
        sender_id: senderId,
        sender_username: "bob",
      });
    },
    { channelId: CHANNEL_ID, id: idAt(index), index, senderId: "u-bob" },
  );
}

for (const skin of SKINS) {
  test.describe(`jump-to-message gap (#1039) — ${skin} skin`, () => {
    test("scrolling forward from a jump renders every message up to the live tail", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await jumpTo(page, TARGET);

      // The jump landed in a window of its own: the live page is not in the
      // document, and neither is anything from the gap. On the unfixed build
      // the live page was merged in and would be a wheel or two away.
      await expect(page.getByTestId(`message-${NEWEST}`)).toHaveCount(0);
      await expect(page.getByTestId(`message-${idAt(TARGET + PAGE + 1)}`)).toHaveCount(0);

      const seen = await wheelForwardTo(page, NEWEST);
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();

      // Every message between the target and the present was rendered on the
      // way — the gap was paged through, not skipped.
      const missing: number[] = [];
      for (let i = TARGET; i < TOTAL; i++) {
        if (!seen.has(i)) {
          missing.push(i);
        }
      }
      expect(missing).toEqual([]);

      // Still a window, not the whole history in the document.
      expect((await renderedIndices(page)).length).toBeLessThan(80);
    });

    test("the same walk from a search hit renders every message up to the live tail", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await jumpViaSearchHit(page, TARGET);
      await expect(page.getByTestId(`message-${NEWEST}`)).toHaveCount(0);

      const seen = await wheelForwardTo(page, NEWEST);
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();

      const missing: number[] = [];
      for (let i = TARGET; i < TOTAL; i++) {
        if (!seen.has(i)) {
          missing.push(i);
        }
      }
      expect(missing).toEqual([]);
    });

    test("having rejoined the tail, the log is live again: an arrival follows at the bottom", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await jumpTo(page, TARGET);
      await wheelForwardTo(page, NEWEST);
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();

      await arrive(page, TOTAL);
      const arrived = page.getByTestId(`message-${idAt(TOTAL)}`);
      await expect(arrived).toBeVisible();
      await expect(arrived).toContainText(`message number ${TOTAL}`);
      // Below the previous newest, i.e. the log is the real tail and the
      // arrival was followed down to.
      const newestBox = await page.getByTestId(`message-${NEWEST}`).boundingBox();
      const arrivedBox = await arrived.boundingBox();
      expect(newestBox).not.toBeNull();
      expect(arrivedBox).not.toBeNull();
      expect((arrivedBox?.y ?? 0)).toBeGreaterThan(newestBox?.y ?? 0);
    });

    test("an arrival while the window is still detached does not drag the reader to it", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await jumpTo(page, TARGET);

      const before = await settledBox(page, `message-${idAt(TARGET)}`);
      await arrive(page, TOTAL);
      // The live page picked the arrival up, but the reader is three pages
      // back in a window that has not reached it: the target stays put and
      // the arrival stays out of the document.
      await page.waitForTimeout(300);
      const after = await page.getByTestId(`message-${idAt(TARGET)}`).boundingBox();
      expect(before).not.toBeNull();
      expect(after).not.toBeNull();
      expect(Math.abs((after?.y ?? 0) - (before?.y ?? 0))).toBeLessThan(2);
      await expect(page.getByTestId(`message-${idAt(TOTAL)}`)).toHaveCount(0);
    });

    test("sending from inside the window returns the log to the present", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await jumpTo(page, TARGET);
      await expect(page.getByTestId(`message-${NEWEST}`)).toHaveCount(0);

      const composer = page.getByTestId("message-input");
      await composer.click();
      await composer.fill("back to the present");
      await composer.press("Enter");

      // Slack does the same: your own message is where the conversation is
      // now, so that is where the log goes. The window's pages are gone with
      // it — they were not contiguous with the tail.
      await expect(page.getByText("back to the present")).toBeVisible();
      await expect(page.getByTestId(`message-${NEWEST}`)).toBeVisible();
      await expect(page.getByTestId(`message-${idAt(TARGET)}`)).toHaveCount(0);
    });
  });
}
