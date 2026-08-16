/*
 * Saved messages + message permalinks (#854) — visual/behavioural e2e.
 *
 * Runs against the browser-rendered frontend with the Tauri IPC mock
 * (`frontend/src/__mocks__/tauri-core.ts`), which reimplements the bookmark and
 * permalink commands with the same semantics as
 * `pollis-core/src/commands/bookmarks.rs` — in particular, permalink resolution
 * consults only this device's own message store.
 *
 * Every test runs in BOTH skins, because the owner asked specifically that
 * visual features stay consistent across `terminal` and `refined`.
 *
 *   pnpm --filter @pollis/e2e playwright
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";

const CHANNEL_MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0MSG";
const DM_MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMM";

// A permalink to a conversation this device is not in. In production the
// message never decrypted (no MLS key material), so it was never written
// locally — the mock's empty conversation is exactly that state.
const FOREIGN_PERMALINK =
  "pollis://m/01HQ7Z3K9M2P5R8T1V4W6Y0FGN/01HQ7Z3K9M2P5R8T1V4W6Y0FGM";

const CHANNEL_PERMALINK = `pollis://m/${CHANNEL_ID}/${CHANNEL_MESSAGE_ID}`;
const DM_PERMALINK = `pollis://m/${DM_ID}/${DM_MESSAGE_ID}`;

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadState(skin: Skin, bookmarks: unknown[] = []) {
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
    dmChannels: [
      {
        id: DM_ID,
        created_by: USER.id,
        created_at: new Date().toISOString(),
        members: [
          { user_id: USER.id, username: "alice", added_by: USER.id, added_at: "" },
          { user_id: "u-bob", username: "bob", added_by: USER.id, added_at: "" },
        ],
      },
    ],
    messages: {
      [CHANNEL_ID]: [
        {
          id: "01HQ7Z3K9M2P5R8T1V4W6Y0OLD",
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "an earlier channel message",
          sent_at: "2026-08-01T10:00:00.000Z",
        },
        {
          id: CHANNEL_MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "the channel message worth saving",
          sent_at: "2026-08-01T10:05:00.000Z",
        },
      ],
      [DM_ID]: [
        {
          id: DM_MESSAGE_ID,
          conversation_id: DM_ID,
          sender_id: "u-bob",
          content: "the dm message worth saving",
          sent_at: "2026-08-01T11:00:00.000Z",
        },
      ],
    },
    bookmarks,
  };
}

/*
 * NOTE ON NAVIGATION: the router runs on `createMemoryHistory` (see
 * `frontend/src/router.tsx`), so the browser URL stays at "/" no matter where
 * the app navigates. `page.goto("/saved")` and `waitForURL` would therefore be
 * meaningless here — every assertion below navigates through the UI and checks
 * what is RENDERED instead.
 */
async function boot(
  page: Page,
  skin: Skin,
  bookmarks: unknown[] = [],
  opts: { failClipboard?: boolean } = {},
) {
  const state = { ...preloadState(skin, bookmarks), ...opts };
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    // `@tauri-apps/api/window` is NOT vite-aliased (only `/core` and `/event`
    // are), so the real module runs and reads `__TAURI_INTERNALS__` directly.
    // It must exist before any app module evaluates, hence addInitScript.
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
  // The sidebar is the signal that the app got past auth/PIN into the shell.
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
 * Navigate to the saved-items page through Cmd+K, which also proves the page is
 * registered in `PAGE_RESULTS` (one of the three places a new static page must
 * be registered).
 */
async function gotoSaved(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("Saved");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("saved-page")).toBeVisible();
}

/** Navigate to the seeded channel through the sidebar. */
async function gotoChannel(page: Page) {
  await openCommandPalette(page);
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`)).toBeVisible();
}

for (const skin of SKINS) {
  test.describe(`bookmarks + permalinks — ${skin} skin`, () => {
    test("saved list shows a saved message and can unsave it", async ({ page }) => {
      await boot(page, skin, [
        {
          message_id: CHANNEL_MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          saved_at: "2026-08-02T09:00:00.000Z",
        },
      ]);

      await gotoSaved(page);

      const row = page.getByTestId(`saved-${CHANNEL_MESSAGE_ID}`);
      await expect(row).toBeVisible();
      await expect(
        page.getByTestId(`saved-content-${CHANNEL_MESSAGE_ID}`),
      ).toHaveText("the channel message worth saving");

      await page.screenshot({
        path: `artifacts/saved-list-${skin}.png`,
        fullPage: true,
      });

      await page.getByTestId(`unsave-${CHANNEL_MESSAGE_ID}`).click();
      await expect(row).toHaveCount(0);
    });

    test("a saved message whose body this device no longer holds says so honestly", async ({
      page,
    }) => {
      // Bookmark pointing at a message that is not in any conversation —
      // i.e. evicted by retention, or never held.
      await boot(page, skin, [
        {
          message_id: "01HQ7Z3K9M2P5R8T1V4W6Y0GNE",
          conversation_id: CHANNEL_ID,
          saved_at: "2026-08-02T09:00:00.000Z",
        },
      ]);

      await gotoSaved(page);
      const placeholder = page.getByTestId(
        "saved-unavailable-01HQ7Z3K9M2P5R8T1V4W6Y0GNE",
      );
      await expect(placeholder).toBeVisible();
      await expect(placeholder).toHaveText(
        "You do not have this message on this device.",
      );

      // The row must not carry a sender, a body, or a timestamp for the message.
      const rowText = await page
        .getByTestId("saved-01HQ7Z3K9M2P5R8T1V4W6Y0GNE")
        .innerText();
      expect(rowText).not.toContain("bob");
      expect(rowText).not.toContain("worth saving");

      await page.screenshot({
        path: `artifacts/saved-unavailable-${skin}.png`,
        fullPage: true,
      });
    });

    test("a permalink to a CHANNEL message jumps to it and highlights it", async ({
      page,
    }) => {
      await boot(page, skin);
      await watchForFlash(page);
      await openCommandPalette(page);
      await page.getByTestId("search-panel-input").fill(CHANNEL_PERMALINK);

      // Landed in the CHANNEL, not the DM — the bug this must not inherit.
      // Proven by what rendered: the channel's messages are on screen and the
      // DM's are not, and the breadcrumb names the channel.
      const target = page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`);
      await expect(target).toBeVisible();
      await expect
        .poll(() => flashedTestIds(page))
        .toContain(`message-${CHANNEL_MESSAGE_ID}`);
      await expect(
        page.getByTestId(`message-01HQ7Z3K9M2P5R8T1V4W6Y0OLD`),
      ).toBeVisible();
      await expect(page.getByTestId(`message-${DM_MESSAGE_ID}`)).toHaveCount(0);
      await expect(page.getByTestId("breadcrumb-trail")).toContainText("general");

      await page.screenshot({
        path: `artifacts/permalink-jump-channel-${skin}.png`,
        fullPage: true,
      });
    });

    test("a permalink to a DM message jumps to the DM route", async ({ page }) => {
      await boot(page, skin);
      await watchForFlash(page);
      await openCommandPalette(page);
      await page.getByTestId("search-panel-input").fill(DM_PERMALINK);

      const target = page.getByTestId(`message-${DM_MESSAGE_ID}`);
      await expect(target).toBeVisible();
      await expect
        .poll(() => flashedTestIds(page))
        .toContain(`message-${DM_MESSAGE_ID}`);
      // The DM's messages are on screen and the channel's are not.
      await expect(
        page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`),
      ).toHaveCount(0);
      await expect(page.getByTestId("breadcrumb-trail")).toContainText("bob");

      await page.screenshot({
        path: `artifacts/permalink-jump-dm-${skin}.png`,
        fullPage: true,
      });
    });

    test("a permalink this device cannot resolve leaks nothing", async ({ page }) => {
      await boot(page, skin);

      await openCommandPalette(page);
      await page.getByTestId("search-panel-input").fill(FOREIGN_PERMALINK);

      const notice = page.getByTestId("permalink-unresolved");
      await expect(notice).toBeVisible();
      await expect(notice).toHaveText(
        "You do not have this message on this device.",
      );

      // It must not navigate — landing the user in the conversation would
      // itself be a signal that the target exists. The palette is still open
      // and no message timeline was opened behind it.
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await expect(page.getByTestId("message-list")).toHaveCount(0);

      // Nothing about the target is rendered: no sender, no content, no
      // claim about whether it exists elsewhere.
      const panelText = await page.getByTestId("search-panel").innerText();
      for (const leak of ["bob", "alice", "Acme", "general", "worth saving"]) {
        expect(panelText).not.toContain(leak);
      }
      expect(panelText).not.toContain("deleted");
      expect(panelText).not.toContain("permission");
      expect(panelText).not.toContain("access");

      await page.screenshot({
        path: `artifacts/permalink-unresolved-${skin}.png`,
        fullPage: true,
      });
    });

    test("copy link puts a bare pollis://m/ pointer on the clipboard", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // Copy-link lives in the per-message "more" menu — open it first.
      const message = page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`);
      await message.getByTestId("message-actions-more").click();
      const copyButton = message.getByTestId("copy-link-button");

      await copyButton.click();
      const clipboard = await page.evaluate(
        () =>
          (window as unknown as { __tauriMock: { clipboard: string } })
            .__tauriMock.clipboard,
      );
      expect(clipboard).toBe(CHANNEL_PERMALINK);
      // The pointer carries the two ids and nothing else.
      expect(clipboard).not.toContain("Acme");
      expect(clipboard).not.toContain("general");
      expect(clipboard).not.toContain("bob");
    });

    test("a successful copy confirms itself on the button", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // Copy-link lives in the per-message "more" menu — open it first. The
      // menu deliberately stays open after the click so the copied/failed
      // feedback is visible where the click happened.
      const message = page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`);
      await message.getByTestId("message-actions-more").click();
      const copyButton = message.getByTestId("copy-link-button");

      await expect(copyButton).toHaveAttribute("data-copy-state", "idle");
      await copyButton.click();

      // Every other copy affordance people use confirms itself; a silent
      // success reads as "the button is broken" (#889).
      await expect(copyButton).toHaveAttribute("data-copy-state", "copied");
      await expect(copyButton).toHaveAccessibleName("Link copied");

      // And it goes back on its own rather than sticking.
      await expect(copyButton).toHaveAttribute("data-copy-state", "idle", {
        timeout: 5000,
      });
    });

    test("a failed copy is distinguishable from a successful one", async ({
      page,
    }) => {
      await boot(page, skin, [], { failClipboard: true });
      await gotoChannel(page);

      // Copy-link lives in the per-message "more" menu — open it first.
      const message = page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`);
      await message.getByTestId("message-actions-more").click();
      const copyButton = message.getByTestId("copy-link-button");

      await copyButton.click();

      // The whole point of #889: the failure must not look like the success.
      await expect(copyButton).toHaveAttribute("data-copy-state", "failed");
      await expect(copyButton).toHaveAccessibleName("Couldn't copy link");

      // Nothing was put on the clipboard, so the user has nothing to paste —
      // which is exactly why they need to be told.
      const clipboard = await page.evaluate(
        () =>
          (window as unknown as { __tauriMock: { clipboard: string } })
            .__tauriMock.clipboard,
      );
      expect(clipboard).toBe("");
    });

    test("saving from the message row lands in the saved list", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // Save lives in the per-message "more" menu — open it first.
      const message = page.getByTestId(`message-${CHANNEL_MESSAGE_ID}`);
      await message.getByTestId("message-actions-more").click();
      const saveButton = message.getByTestId("save-button");

      await saveButton.click();
      await gotoSaved(page);
      await expect(
        page.getByTestId(`saved-${CHANNEL_MESSAGE_ID}`),
      ).toBeVisible();
    });
  });
}
