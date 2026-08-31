/*
 * On-device message search (#850) — behavioural e2e.
 *
 * Runs against the browser-rendered frontend with the Tauri IPC mock
 * (`frontend/src/__mocks__/tauri-core.ts`), whose `search_messages` case
 * reproduces the SHAPE of the Rust command — enriched results, structured
 * snippets with UTF-16 highlight ranges, a total count, offset pagination and
 * both orderings. The ranking itself is Rust's job and is pinned by
 * `pollis-core`'s own tests; what this spec pins is the user-visible flow the
 * ticket was actually about:
 *
 *   type a query → ranked results with REAL NAMES (not UUIDs) → click a channel
 *   hit → land on the channel route (not the broken DM route every hit used to
 *   go to) → see the message flash-highlighted.
 *
 * Every test runs in BOTH skins, per the repo convention that visual features
 * stay consistent across `terminal` and `refined`.
 *
 *   pnpm --filter @pollis/e2e playwright
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2XCB";
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2DMX";

// The channel hit. Deliberately NOT the newest message in its channel, so the
// jump has somewhere to jump to.
const CHANNEL_HIT_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2HIT";
const CHANNEL_NEWEST_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2NEW";
const DM_HIT_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2DMM";
const OLD_HIT_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2OLD";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

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
    groupMembers: {
      [GROUP_ID]: [
        { user_id: USER.id, username: "alice", role: "admin", joined_at: "" },
        { user_id: "u-bob", username: "bob", role: "member", joined_at: "" },
      ],
    },
    dmChannels: [
      {
        id: DM_ID,
        created_by: USER.id,
        created_at: new Date().toISOString(),
        members: [
          { user_id: USER.id, username: "alice", added_by: USER.id, added_at: "" },
          { user_id: "u-carol", username: "carol", added_by: USER.id, added_at: "" },
        ],
      },
    ],
    messages: {
      [CHANNEL_ID]: [
        {
          id: OLD_HIT_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "an early note about the budget",
          sent_at: "2026-08-01T09:00:00.000Z",
        },
        {
          id: CHANNEL_HIT_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          // Two occurrences, so relevance puts it above the single-mention rows.
          content: "the budget review is on friday, bring the budget deck",
          sent_at: "2026-08-01T10:00:00.000Z",
        },
        {
          id: CHANNEL_NEWEST_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-alice",
          content: "sounds good, see you there",
          sent_at: "2026-08-02T10:00:00.000Z",
        },
      ],
      [DM_ID]: [
        {
          id: DM_HIT_ID,
          conversation_id: DM_ID,
          sender_id: "u-carol",
          content: "did you see the budget?",
          sent_at: "2026-08-03T10:00:00.000Z",
        },
      ],
    },
  };
}

/*
 * NOTE ON NAVIGATION: the router runs on `createMemoryHistory` (see
 * `frontend/src/router.tsx`), so the browser URL stays at "/" no matter where
 * the app navigates. Every assertion below checks what is RENDERED.
 */
async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    // `@tauri-apps/api/window` is NOT vite-aliased (only `/core` and `/event`
    // are), so the real module runs and reads `__TAURI_INTERNALS__` directly.
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

/**
 * Record every element that receives the jump-highlight class.
 *
 * Asserting `toHaveClass` directly races the animation — the class is removed
 * after HIGHLIGHT_MS, so a slow first poll sees a clean element and fails even
 * though the flash happened. Observing mutations captures the event itself.
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

async function openCommandPalette(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
}

/**
 * Type into the palette and wait for the value to stick.
 *
 * The panel clears its own input in an effect keyed on `isOpen`, so a `fill`
 * that lands in the same tick as the open can be wiped a moment later. Waiting
 * on the value rather than on the panel's visibility is what makes that
 * deterministic.
 */
async function typeInPalette(page: Page, query: string) {
  const input = page.getByTestId("search-panel-input");
  await input.fill(query);
  await expect(input).toHaveValue(query);
}

/** Open `/search` and run a query. Goes through Cmd+K's page result, which is
 *  also what proves `/search` is registered in `PAGE_RESULTS`. */
async function search(page: Page, query: string) {
  await openCommandPalette(page);
  await typeInPalette(page, "Search Messages");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("search-view")).toBeVisible();
  await page.getByTestId("search-input").fill(query);
}

for (const skin of SKINS) {
  test.describe(`message search — ${skin} skin`, () => {
    test("a query returns ranked results carrying real names, not ids", async ({ page }) => {
      await boot(page, skin);
      await search(page, "budget");

      const items = page.getByTestId("search-result-item");
      await expect(items).toHaveCount(3);

      // Names, not UUIDs — the first of the three original defects. `u-bob`
      // and the raw conversation id must not appear anywhere in the list.
      await expect(page.getByTestId("search-result-sender").first()).toHaveText("bob");
      const conversations = await page
        .getByTestId("search-result-conversation")
        .allTextContents();
      for (const label of conversations) {
        expect(label).not.toContain(CHANNEL_ID);
        expect(label).not.toContain(DM_ID);
      }
      expect(conversations).toContain("Acme / #general");
      expect(conversations).toContain("@carol");

      // The query term is marked from the ranges the backend returned, not by
      // a regex in the renderer.
      await expect(page.getByTestId("search-highlight").first()).toHaveText("budget");

      // "About N results" — the count is of ALL hits, not of the page.
      await expect(page.getByTestId("search-total")).toHaveText("About 3 results");
    });

    test("the sort toggle reorders the results", async ({ page }) => {
      await boot(page, skin);
      await search(page, "budget");
      await expect(page.getByTestId("search-result-item")).toHaveCount(3);

      // A global search defaults to relevance: the two-mention channel message
      // outranks the newer single-mention DM.
      const relevanceFirst = await page
        .getByTestId("search-result-item")
        .first()
        .getAttribute("data-conversation-kind");
      expect(relevanceFirst).toBe("channel");

      await page.getByTestId("search-sort-recent").click();
      await expect(
        page.getByTestId("search-result-item").first(),
      ).toHaveAttribute("data-conversation-kind", "dm");
    });

    test("clicking a channel hit lands on the channel route and flashes the message", async ({
      page,
    }) => {
      await boot(page, skin);
      await watchForFlash(page);
      await search(page, "budget");

      const channelHit = page
        .getByTestId("search-result-item")
        .filter({ hasText: "Acme / #general" })
        .first();
      await expect(channelHit).toBeVisible();
      await channelHit.click();

      // The channel route, NOT `/dms/$conversationId` — every channel hit used
      // to land on a broken DM route.
      await expect(page.getByTestId("main-content")).toBeVisible();
      await expect(page.getByTestId(`message-${CHANNEL_HIT_ID}`)).toBeVisible();
      // The breadcrumb is what says which route actually matched.
      await expect(page.getByTestId("breadcrumb-nav")).toContainText("general");

      await expect
        .poll(() => flashedTestIds(page))
        .toContain(`message-${CHANNEL_HIT_ID}`);
    });

    test("a DM hit lands on the DM route", async ({ page }) => {
      await boot(page, skin);
      await watchForFlash(page);
      await search(page, "budget");

      const dmHit = page
        .getByTestId("search-result-item")
        .filter({ hasText: "@carol" })
        .first();
      await dmHit.click();

      await expect(page.getByTestId(`message-${DM_HIT_ID}`)).toBeVisible();
      await expect
        .poll(() => flashedTestIds(page))
        .toContain(`message-${DM_HIT_ID}`);
    });

    test("Cmd+K hands the query off to the search page instead of listing messages", async ({
      page,
    }) => {
      await boot(page, skin);
      await openCommandPalette(page);
      await typeInPalette(page, "budget");

      // Cmd+K stays a NAVIGATOR: no message hits render inline.
      await expect(page.getByTestId("search-panel")).not.toContainText(
        "the budget review is on friday",
      );

      // ...but it offers the door to the search page, carrying the query.
      const handoff = page
        .getByTestId("search-panel-result-item")
        .filter({ hasText: 'Search messages for "budget"' });
      await expect(handoff).toBeVisible();
      await handoff.click();

      await expect(page.getByTestId("search-view")).toBeVisible();
      // The handed-off query arrives already run — no retyping.
      await expect(page.getByTestId("search-input")).toHaveValue("budget");
      await expect(page.getByTestId("search-result-item")).toHaveCount(3);
    });

    test("the results footer states the corpus, and the empty state explains why", async ({
      page,
    }) => {
      await boot(page, skin);
      await search(page, "budget");

      // The corpus footer is persistent and says what search can actually see.
      await expect(page.getByTestId("search-corpus-footer")).toContainText(
        "4 messages stored on this device",
      );
      await expect(page.getByTestId("search-learn-link")).toBeVisible();

      // "No results" is not an answer on its own — the reasons a message can be
      // missing are properties of end-to-end encryption, not of the query.
      await page.getByTestId("search-input").fill("nothingmatchesthis");
      const empty = page.getByTestId("search-no-results");
      await expect(empty).toBeVisible();
      await expect(empty).toContainText("have not opened on this device");
      await expect(empty).toContainText("before you joined");
      await expect(empty).toContainText("before this device was added");
    });

    test("a from: filter narrows the results", async ({ page }) => {
      await boot(page, skin);
      await search(page, "budget from:@carol");
      await expect(page.getByTestId("search-result-item")).toHaveCount(1);
      await expect(page.getByTestId("search-result-sender")).toHaveText("carol");
    });
  });
}
