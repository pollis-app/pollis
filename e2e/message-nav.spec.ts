/*
 * Arrow-key chat-log navigation — browser-level e2e.
 *
 * The pure state machine is pinned by `frontend/tests/message-nav.test.ts`;
 * THIS spec proves the DOM half: the composer hands ArrowUp off at the right
 * caret positions, logical focus projects onto real element focus (rows, then
 * action-bar buttons), and the exits (down past newest, Escape, Tab) land back
 * in the composer. Runs against the browser build with the Tauri IPC mock,
 * in both skins, like the other UI specs.
 *
 *   pnpm --filter @pollis/e2e e2e:ui message-nav
 */

import { test, expect, type Page } from "@playwright/test";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1XCB";

const OLD_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1OLD";
const MID_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1MID";
// The newest message is the viewer's own, so its bar carries reply+edit+more.
const OWN_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1OWN";

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
    dmChannels: [],
    messages: {
      [CHANNEL_ID]: [
        {
          id: OLD_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "the oldest message",
          sent_at: "2026-08-01T10:00:00.000Z",
        },
        {
          id: MID_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "the middle message",
          sent_at: "2026-08-01T10:05:00.000Z",
        },
        {
          id: OWN_ID,
          conversation_id: CHANNEL_ID,
          sender_id: USER.id,
          content: "my own newest message",
          sent_at: "2026-08-01T10:10:00.000Z",
        },
      ],
    },
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
  // Deterministic starting point: the composer owns focus.
  await page.getByTestId("message-input").click();
  await expect(page.getByTestId("message-input")).toBeFocused();
}

for (const skin of SKINS) {
  test.describe(`arrow-key log navigation — ${skin} skin`, () => {
    test("ArrowUp from an empty composer focuses the newest message; ArrowDown returns", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OWN_ID}`)).toBeFocused();

      // Walk older, clamp at the oldest.
      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${MID_ID}`)).toBeFocused();
      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OLD_ID}`)).toBeFocused();
      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OLD_ID}`)).toBeFocused();

      // Walk back down; past the newest the composer takes focus again.
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("ArrowDown");
      await expect(page.getByTestId(`message-${OWN_ID}`)).toBeFocused();
      await page.keyboard.press("ArrowDown");
      await expect(page.getByTestId("message-input")).toBeFocused();
    });

    test("Left/Right walk the focused row's action bar; Enter activates", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);

      await page.keyboard.press("ArrowUp");
      const ownRow = page.getByTestId(`message-${OWN_ID}`);
      await expect(ownRow).toBeFocused();

      // Own message: reply → edit → more, clamping at the end.
      await page.keyboard.press("ArrowRight");
      await expect(ownRow.getByTestId("reply-button")).toBeFocused();
      await page.keyboard.press("ArrowRight");
      await expect(ownRow.getByTestId("edit-button")).toBeFocused();
      await page.keyboard.press("ArrowRight");
      await expect(ownRow.getByTestId("message-actions-more")).toBeFocused();
      await page.keyboard.press("ArrowRight");
      await expect(ownRow.getByTestId("message-actions-more")).toBeFocused();

      // Back past the first action returns to the bare row.
      await page.keyboard.press("ArrowLeft");
      await page.keyboard.press("ArrowLeft");
      await expect(ownRow.getByTestId("reply-button")).toBeFocused();
      await page.keyboard.press("ArrowLeft");
      await expect(ownRow).toBeFocused();

      // Enter on "more" opens the row's action menu.
      await page.keyboard.press("ArrowRight");
      await page.keyboard.press("ArrowRight");
      await page.keyboard.press("ArrowRight");
      await page.keyboard.press("Enter");
      await expect(ownRow.getByTestId("message-actions-menu")).toBeVisible();
      // Its own Escape closes the menu without ending navigation…
      await page.keyboard.press("Escape");
      await expect(ownRow.getByTestId("message-actions-menu")).toHaveCount(0);
      // …and a second Escape exits back to the composer.
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("message-input")).toBeFocused();
    });

    test("Tab exits navigation back to the composer", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OWN_ID}`)).toBeFocused();
      await page.keyboard.press("Tab");
      await expect(page.getByTestId("message-input")).toBeFocused();
    });

    test("the composer only hands off with the caret on the first line", async ({
      page,
    }) => {
      await boot(page, skin);
      await gotoChannel(page);
      const input = page.getByTestId("message-input");

      // Two lines, caret at the end (second line): ArrowUp is native caret
      // movement, not a hand-off.
      await input.fill("first line\nsecond line");
      await page.keyboard.press("ArrowUp");
      await expect(input).toBeFocused();

      // Caret moved up to the first line: now ArrowUp hands off — with the
      // draft preserved.
      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OWN_ID}`)).toBeFocused();
      await expect(input).toHaveValue("first line\nsecond line");
    });
  });
}
