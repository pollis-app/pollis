// One confirmation pattern and one form pattern on desktop (#1968), both skins. A confirmation page
// (Leave group, Remove member, the leave half of Conversation settings, the Security page's in-place
// revoke) is the page column, top-aligned: the question, its consequence, the one `danger` button
// and a Cancel that is the default focus and returns where the user came from. A form page (Create
// group, Find a group, Rename group, New message, Join a group, Change PIN) is the same column, at
// most 420px, with no card around it and its action at its natural width. Screenshots of every
// touched page land in `e2e/artifacts/confirm-form/` for review.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_form";
const CHANNEL_ID = "c_general";
const DM_ID = "dm_bob";
const NOW = "2026-11-01T09:00:00.000Z";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const SHOTS = "artifacts/confirm-form";
// `max-w-md` is 28rem: 420px at the 15px root. One pixel of rounding slack.
const COLUMN_MAX = 421;
// The column starts within its pane's top padding (2rem), never centred down the pane.
const TOP_SLACK = 48;

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "Studio", owner_id: ME.id, created_at: NOW, current_user_role: "admin" },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" }],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: NOW },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: NOW },
      ],
    },
    dmChannels: [
      {
        id: DM_ID,
        created_by: ME.id,
        created_at: NOW,
        members: [
          { user_id: ME.id, username: "mia", added_by: ME.id, added_at: NOW },
          { user_id: "u_bob", username: "bob", added_by: ME.id, added_at: NOW },
        ],
      },
    ],
    messages: {
      [DM_ID]: [{ id: "m_hi", conversation_id: DM_ID, sender_id: "u_bob", content: "hi", sent_at: NOW }],
    },
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.addStyleTag({
    content: "*, *::before, *::after { transition: none !important; animation: none !important; }",
  });
}

async function openGroup(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await expect(page.getByTestId("menu-item-leave-group")).toBeVisible();
}

async function openPage(page: Page, name: string, testId: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(name);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(testId)).toBeVisible();
}

async function openSecurity(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill("Security");
  await page.getByTestId("search-panel-result-item").filter({ hasText: "/security" }).first().click();
  await expect(page.getByTestId("security-page")).toBeVisible();
}

interface ColumnFacts {
  width: number;
  /** Column top minus the page title bar's bottom. */
  offsetTop: number;
  /** What makes the column a card: a border or a fill of its own. */
  boxed: string[];
}

/** Layout facts for the page column that holds `selector` (the column or anything inside it). */
async function columnFacts(page: Page, selector: string): Promise<ColumnFacts> {
  return page.evaluate((selector) => {
    const inner = document.querySelector<HTMLElement>(selector);
    if (!inner) {
      throw new Error(`missing ${selector}`);
    }
    // The column is the child of the scrolling pane, the nearest `overflow: auto` ancestor.
    let column: HTMLElement = inner;
    while (column.parentElement && getComputedStyle(column.parentElement).overflowY !== "auto") {
      column = column.parentElement;
    }
    const title = document.querySelector<HTMLElement>('[data-testid="page-title"]');
    const titleBottom = title ? title.getBoundingClientRect().bottom : 0;
    // A card is a border or a fill on the column itself (Join a group's old form was one).
    const cs = getComputedStyle(column);
    const boxed: string[] = [];
    if (parseFloat(cs.borderTopWidth) > 0 && cs.borderTopStyle !== "none") {
      boxed.push("border");
    }
    if (cs.backgroundColor !== "rgba(0, 0, 0, 0)" && cs.backgroundColor !== "transparent") {
      boxed.push("background");
    }
    const rect = column.getBoundingClientRect();
    return { width: rect.width, offsetTop: rect.top - titleBottom, boxed };
  }, selector);
}

/** The `danger`-filled buttons in the main pane. */
async function dangerButtons(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLButtonElement>("button"))
      .filter((b) => b.className.includes("bg-danger") && b.getClientRects().length > 0)
      .map((b) => b.dataset.testid ?? b.textContent ?? ""),
  ).then((ids) => Array.from(new Set(ids)));
}

async function expectConfirmation(page: Page, blockTestId: string, confirmTestId: string) {
  const block = page.getByTestId(blockTestId);
  await expect(block).toBeVisible();
  const facts = await columnFacts(page, `[data-testid="${blockTestId}"]`);
  expect(facts.width).toBeLessThanOrEqual(COLUMN_MAX);
  expect(facts.offsetTop).toBeLessThan(TOP_SLACK);
  expect(facts.boxed).toEqual([]);
  // A question and its consequence, then the one danger button.
  await expect(block.locator("p").first()).not.toBeEmpty();
  expect(await dangerButtons(page)).toEqual([confirmTestId]);
  // The action is never the default focus.
  await page.waitForTimeout(150);
  const focused = await page.evaluate(() => (document.activeElement as HTMLElement | null)?.dataset.testid ?? null);
  expect(focused).not.toBe(confirmTestId);
}

async function expectForm(page: Page, inner: string, submitTestId?: string) {
  const facts = await columnFacts(page, inner);
  expect(facts.width).toBeLessThanOrEqual(COLUMN_MAX);
  expect(facts.offsetTop).toBeLessThan(TOP_SLACK);
  // No card around the form.
  expect(facts.boxed).toEqual([]);
  if (submitTestId) {
    const submit = await page.getByTestId(submitTestId).boundingBox();
    expect(submit?.width ?? 0).toBeLessThan(facts.width);
  }
}

for (const skin of SKINS) {
  test.describe(`confirm and form patterns — ${skin}`, () => {
    test("Leave group: the confirmation pattern; Cancel returns to the group", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId("menu-item-leave-group").click();
      await expectConfirmation(page, "leave-group-confirmation", "leave-group-confirm");
      await expect(page.getByTestId("leave-group-cancel")).toBeFocused();
      await expect(page.getByTestId("leave-group-confirmation")).toContainText("Studio");
      await page.screenshot({ path: `${SHOTS}/${skin}-leave-group.png` });
      await page.getByTestId("leave-group-cancel").click();
      await expect(page.getByTestId("menu-item-leave-group")).toBeVisible();
    });

    test("Delete channel: the bar replaces the composer and focuses its X, never Delete", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
      await page.getByTestId("delete-channel-trigger").click();
      await expect(page.getByTestId("delete-channel-bar")).toBeVisible();
      // Enter must not delete by accident: the safe choice holds focus.
      await expect(page.getByTestId("delete-channel-cancel")).toBeFocused();
      await page.keyboard.press("Enter");
      await expect(page.getByTestId("delete-channel-bar")).toHaveCount(0);
    });

    test("Remove member: the confirmation pattern; Cancel returns to Members", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId("menu-item-members").click();
      await page.getByTestId("member-kick-u_dana").click();
      await expectConfirmation(page, "kick-member-confirmation", "kick-member-confirm");
      await expect(page.getByTestId("kick-member-cancel")).toBeFocused();
      await expect(page.getByTestId("kick-member-confirmation")).toContainText("dana");
      await page.screenshot({ path: `${SHOTS}/${skin}-remove-member.png` });
      await page.getByTestId("kick-member-cancel").click();
      await expect(page.getByTestId("member-kick-u_dana")).toBeVisible();
    });

    test("Conversation settings: export, then the leave confirmation; Cancel returns to the DM", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-dms").click();
      await page.getByTestId(`dm-option-${DM_ID}-secondary`).click();
      await expectConfirmation(page, "dm-settings-leave", "dm-settings-leave-button");
      // Not a centred stack of full-width outlined buttons: each at its natural width.
      const leave = await page.getByTestId("dm-settings-leave-button").boundingBox();
      const facts = await columnFacts(page, '[data-testid="dm-settings-leave"]');
      expect(leave?.width ?? 0).toBeLessThan(facts.width);
      await expect(page.getByTestId("dm-settings-leave")).toContainText("@bob");
      await page.screenshot({ path: `${SHOTS}/${skin}-dm-settings.png` });
      await page.getByTestId("dm-settings-cancel-button").click();
      await expect(page.getByTestId("dm-settings-leave")).toHaveCount(0);
      await expect(page.getByTestId("message-form")).toBeVisible();
    });

    test("Security: revoking media access confirms in the column with the same pattern", async ({ page }) => {
      await boot(page, skin);
      await openSecurity(page);
      await page.getByRole("button", { name: "Revoke now" }).click();
      const block = page.getByTestId("revoke-media-confirm");
      await expect(block).toBeVisible();
      await expect(block.locator("button.bg-danger, button[class*='bg-danger']")).toHaveCount(1);
      await page.waitForTimeout(150);
      await expect(block.getByRole("button", { name: "Cancel" })).toBeFocused();
      await block.scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}/${skin}-security-revoke-media.png` });
      await block.getByRole("button", { name: "Cancel" }).click();
      await expect(block).toHaveCount(0);
      await expect(page.getByRole("button", { name: "Revoke now" })).toBeVisible();
    });

    test("Join a group: the form column, no card", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-join").click();
      await expect(page.getByTestId("join-by-invite-page")).toBeVisible();
      await expectForm(page, '[data-testid="invite-token-input"]', "redeem-invite-link");
      await page.screenshot({ path: `${SHOTS}/${skin}-join-group.png` });
    });

    test("Create group: the form column; the submit at its natural width", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "create group", "create-group-form");
      await expectForm(page, '[data-testid="create-group-form"]', "create-group-submit-button");
      await page.screenshot({ path: `${SHOTS}/${skin}-create-group.png` });
    });

    test("Find a group: the form column", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-groups").click();
      await page.getByTestId("menu-item-find-group").click();
      await expect(page.getByTestId("search-group-page")).toBeVisible();
      await expectForm(page, '[data-testid="search-group-slug-input"]', "search-group-button");
      await page.screenshot({ path: `${SHOTS}/${skin}-find-group.png` });
    });

    test("Rename group: the form column", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId("menu-item-rename-group").click();
      await expect(page.getByTestId("rename-group-form")).toBeVisible();
      await expectForm(page, '[data-testid="rename-group-form"]', "rename-group-submit-button");
      await page.screenshot({ path: `${SHOTS}/${skin}-rename-group.png` });
    });

    test("New message: the form column", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("menu-item-dms").click();
      await page.getByTestId("menu-item-new-dm").click();
      await expect(page.getByTestId("start-dm-form")).toBeVisible();
      await expectForm(page, '[data-testid="start-dm-form"]', "start-dm-submit-button");
      await page.screenshot({ path: `${SHOTS}/${skin}-new-message.png` });
    });

    test("Change PIN: the form column; Cancel returns to Security", async ({ page }) => {
      await boot(page, skin);
      await openSecurity(page);
      await page.getByTestId("change-pin-button").click();
      await expect(page.getByTestId("change-pin-page")).toBeVisible();
      await expectForm(page, '[data-testid="change-pin-page"]');
      await page.screenshot({ path: `${SHOTS}/${skin}-change-pin.png` });
      await page.getByTestId("change-pin-cancel-button").click();
      await expect(page.getByTestId("security-page")).toBeVisible();
    });
  });
}
