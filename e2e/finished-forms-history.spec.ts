// A finished create/edit page leaves history (#2129), in BOTH skins (BreadcrumbNav is per-skin
// markup). Finishing a form replaces its entry with the result, or steps back to the result when
// that is the page behind the form, so neither Back nor Forward returns to the finished form.
// Back from a form that is still open works as before. The rules are pinned as a pure reducer
// in `frontend/tests/history-nav.test.ts`; mobile's half is Maestro `flows/finished-forms-back.yaml`.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_hist";
const CHANNEL_ID = "c_general";
const NOW = "2026-11-01T09:00:00.000Z";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

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
      [GROUP_ID]: [{ user_id: ME.id, username: "mia", role: "admin", joined_at: NOW }],
    },
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

const back = (page: Page) => page.getByTestId("breadcrumb-back-button");
const forward = (page: Page) => page.getByTestId("breadcrumb-forward-button");

async function openGroupsList(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await expect(page.getByTestId("menu-item-create-group")).toBeVisible();
}

async function openGroup(page: Page) {
  await openGroupsList(page);
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await expect(page.getByTestId("menu-item-rename-group")).toBeVisible();
}

for (const skin of SKINS) {
  test.describe(`finished forms leave history (${skin})`, () => {
    test("create group: Back from the new group returns to the groups list", async ({ page }) => {
      await boot(page, skin);
      await openGroupsList(page);
      await page.getByTestId("menu-item-create-group").click();
      await expect(page.getByTestId("create-group-form")).toBeVisible();

      await page.locator("#create-group-name").fill("Night shift");
      await page.getByTestId("create-group-submit-button").click();

      // Landed on the new group, with nothing ahead of it.
      await expect(page.getByTestId("menu-item-rename-group")).toBeVisible();
      await expect(page.getByTestId("create-group-form")).toHaveCount(0);
      await expect(forward(page)).toBeDisabled();

      // Back skips the finished form.
      await back(page).click();
      await expect(page.getByTestId("menu-item-create-group")).toBeVisible();
      await expect(page.getByTestId("create-group-form")).toHaveCount(0);

      // Forward is the group, not the form.
      await forward(page).click();
      await expect(page.getByTestId("menu-item-rename-group")).toBeVisible();
      await expect(page.getByTestId("create-group-form")).toHaveCount(0);
    });

    test("create channel: Back from the new channel returns to the group", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId("menu-item-create-channel").click();
      await expect(page.getByTestId("create-channel-form")).toBeVisible();

      await page.locator("#create-channel-name").fill("standup");
      await page.getByTestId("create-channel-submit-button").click();
      await expect(page.getByTestId("create-channel-form")).toHaveCount(0);
      await expect(forward(page)).toBeDisabled();

      await back(page).click();
      await expect(page.getByTestId("menu-item-rename-group")).toBeVisible();
      await expect(page.getByTestId("create-channel-form")).toHaveCount(0);
    });

    test("rename group: Save steps back to the group page, leaving no twin and no form ahead", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId("menu-item-rename-group").click();
      await expect(page.getByTestId("rename-group-form")).toBeVisible();

      await page.locator("#rename-group-name").fill("Studio B");
      await page.getByTestId("rename-group-submit-button").click();

      await expect(page.getByTestId("menu-item-rename-group")).toBeVisible();
      await expect(page.getByTestId("rename-group-form")).toHaveCount(0);
      await expect(forward(page)).toBeDisabled();

      // One Back reaches the groups list: the group page is not stacked twice.
      await back(page).click();
      await expect(page.getByTestId("menu-item-create-group")).toBeVisible();
    });

    test("Back from an open form, before it is submitted, works as before", async ({ page }) => {
      await boot(page, skin);
      await openGroup(page);
      await page.getByTestId("menu-item-rename-group").click();
      await expect(page.getByTestId("rename-group-form")).toBeVisible();

      await back(page).click();
      await expect(page.getByTestId("menu-item-rename-group")).toBeVisible();

      // An unfinished form stays reachable by Forward, like any page.
      await expect(forward(page)).toBeEnabled();
      await forward(page).click();
      await expect(page.getByTestId("rename-group-form")).toBeVisible();
    });

    test("Cancel on a form returns to the page it came from and drops the form", async ({ page }) => {
      await boot(page, skin);
      await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await page.getByTestId("search-panel-input").fill("Security");
      await page.getByTestId("search-panel-result-item").filter({ hasText: "/security" }).first().click();
      await expect(page.getByTestId("security-page")).toBeVisible();

      await page.getByTestId("change-pin-button").click();
      await expect(page.getByTestId("change-pin-page")).toBeVisible();
      await page.getByTestId("change-pin-cancel-button").click();

      await expect(page.getByTestId("security-page")).toBeVisible();
      await expect(page.getByTestId("change-pin-page")).toHaveCount(0);
      await expect(forward(page)).toBeDisabled();

      // Back goes on past Security to where it was opened from, not into the cancelled form.
      await back(page).click();
      await expect(page.getByTestId("menu-item-groups")).toBeVisible();
      await expect(page.getByTestId("change-pin-page")).toHaveCount(0);
    });
  });
}
