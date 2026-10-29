/*
 * Act on pointer-down where an accidental press is free to undo (#1529).
 * BOTH skins.
 *
 *   1. A converted control acts while the mouse button is still down, and
 *      the release of the same press does not run it a second time: the
 *      message "more" menu is a toggle (twice = closed again), and the
 *      breadcrumb Back walks history (twice = one entry too far).
 *   2. The same controls still work from the keyboard — Enter and Space
 *      produce a click with `detail === 0`, which always acts.
 *   3. A control that stays on release (the channel mute bell, a
 *      persisted toggle) does nothing on pointer-down, acts on release, and
 *      does nothing when the press is dragged off before release.
 *
 * The rule itself is pure and pinned in `frontend/tests/press-activation.test.ts`.
 */

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_press";
const GENERAL = "c_general";
const RANDOM = "c_random";
const MSG = "m_press_1";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadFor(skin: Skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "Book club",
        owner_id: "u_bob",
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "member",
      },
    ],
    channels: {
      [GROUP_ID]: [
        { id: GENERAL, group_id: GROUP_ID, name: "general" },
        { id: RANDOM, group_id: GROUP_ID, name: "random" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_bob", username: "bob", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {
      [GENERAL]: [
        {
          id: MSG,
          conversation_id: GENERAL,
          sender_id: "u_bob",
          sender_username: "bob",
          content: "press me",
          sent_at: "2026-08-01T10:00:00.000Z",
        },
      ],
      [RANDOM]: [],
    },
    dmChannels: [],
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openChannel(page: Page, name: string) {
  await page.getByTestId("sidebar").getByRole("button", { name: new RegExp(`${name}$`) }).click();
  await expect(page.getByTestId("breadcrumb-trail")).toContainText(name);
}

/** Move the mouse to the centre of `testId`'s element and press (no release). */
async function pressDown(page: Page, locator: ReturnType<Page["getByTestId"]>) {
  const box = await locator.boundingBox();
  if (!box) {
    throw new Error("control is not on screen");
  }
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  return box;
}

for (const skin of SKINS) {
  test.describe(`press activation — ${skin} skin`, () => {
    test("a converted toggle opens on pointer-down and its release does not toggle it back", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, "general");
      const row = page.getByTestId(`message-${MSG}`);
      await row.hover();
      const more = row.getByTestId("message-actions-more");
      await expect(more).toBeVisible();

      await pressDown(page, more);
      // Open while the button is still held.
      await expect(page.getByTestId("message-actions-menu")).toBeVisible();
      await page.mouse.up();
      // The click of the same press did not run the toggle a second time.
      await expect(page.getByTestId("message-actions-menu")).toBeVisible();
      await expect(more).toHaveAttribute("aria-expanded", "true");

      // A second press closes it: one action per press, every press.
      await pressDown(page, more);
      await expect(page.getByTestId("message-actions-menu")).toHaveCount(0);
      await page.mouse.up();
      await expect(page.getByTestId("message-actions-menu")).toHaveCount(0);
    });

    test("Enter and Space still activate a converted control", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, "general");
      const row = page.getByTestId(`message-${MSG}`);
      await row.hover();
      const more = row.getByTestId("message-actions-more");
      await more.focus();
      await page.keyboard.press("Enter");
      await expect(page.getByTestId("message-actions-menu")).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("message-actions-menu")).toHaveCount(0);

      await more.focus();
      await page.keyboard.press("Space");
      await expect(page.getByTestId("message-actions-menu")).toBeVisible();
    });

    test("a mouse press on Back goes back exactly one entry", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, "general");
      await openChannel(page, "random");

      const back = page.getByTestId("breadcrumb-back-button");
      await pressDown(page, back);
      // Already back on general while the button is held…
      await expect(page.getByTestId("breadcrumb-trail")).toContainText("general");
      await page.mouse.up();
      // …and the release did not take a second step back past it.
      await expect(page.getByTestId("breadcrumb-trail")).toContainText("general");
      await expect(page.getByTestId("breadcrumb-trail")).not.toContainText("random");

      // Forward, from the keyboard: Enter, then Space, one entry each.
      const forward = page.getByTestId("breadcrumb-forward-button");
      await forward.focus();
      await page.keyboard.press("Enter");
      await expect(page.getByTestId("breadcrumb-trail")).toContainText("random");
      await back.focus();
      await page.keyboard.press("Space");
      await expect(page.getByTestId("breadcrumb-trail")).toContainText("general");
      await expect(page.getByTestId("breadcrumb-trail")).not.toContainText("random");
    });

    test("search opens on pointer-down with its field focused", async ({ page }) => {
      await boot(page, skin);
      const button = page.getByTestId("breadcrumb-search-button");
      await pressDown(page, button);
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await page.mouse.up();
      // The release over the backdrop did not close it, and the press did
      // not hand focus back to the button.
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await expect(page.getByTestId("search-panel-input")).toBeFocused();
    });

    test("an excluded control still waits for the release", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, "general");
      const toggle = page.getByTestId("channel-mute-toggle");
      await expect(toggle).toHaveAttribute("data-muted", "false");

      // Pressed and held: nothing yet.
      await pressDown(page, toggle);
      await page.waitForTimeout(150);
      await expect(toggle).toHaveAttribute("data-muted", "false");
      // Released on the control: it acts.
      await page.mouse.up();
      await expect(toggle).toHaveAttribute("data-muted", "true");

      // Pressed, then dragged off before release: it never acts.
      const box = await pressDown(page, toggle);
      await page.mouse.move(box.x + box.width / 2, box.y + 200);
      await page.mouse.up();
      await page.waitForTimeout(150);
      await expect(toggle).toHaveAttribute("data-muted", "true");
    });
  });
}
