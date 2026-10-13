/*
 * Group / channel identity for someone who cannot edit it (#1281). BOTH skins.
 *
 * A non-admin must never be shown an empty, editable-looking Description box
 * ("Optional description…") they cannot type into. On desktop every way into
 * the rename page is admin-only, so the two things to prove are:
 *
 *   1. a member is offered no rename entry point, for the group or a channel;
 *   2. the rename page itself, if it is open when the viewer stops being an
 *      admin (a demotion arriving while it is on screen), turns into a
 *      read-only view: the name as text, the description as text only when
 *      there is one, and no Save.
 *
 * An admin's form is unchanged — including an empty, editable Description.
 * The mobile half is `.maestro/two-client/join-request/5-peer-settings.yaml`.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_identity";
const CHANNEL_ID = "c_general";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadFor(skin: Skin, role: "admin" | "member", description?: string) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "Book club",
        description,
        owner_id: "u_other",
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: role,
      },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role, joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: [] },
    dmChannels: [],
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin, role: "admin" | "member", description?: string) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, role, description));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
}

/**
 * Demote the viewer in the mock, then make the app refetch its group list the
 * way the real one does on a `membership_changed` push: the list is 60 s
 * fresh, so jump the clock past that and fire a reconnect, which refetches
 * every stale query.
 */
async function demoteWhileOpen(page: Page) {
  await page.evaluate((groupId) => {
    const store = (window as unknown as {
      __tauriMock: {
        groups: Array<{ id: string; current_user_role?: string }>;
        groupMembers: Record<string, Array<{ role: string }>>;
      };
    }).__tauriMock;
    store.groups.find((g) => g.id === groupId)!.current_user_role = "member";
    for (const m of store.groupMembers[groupId] ?? []) {
      m.role = "member";
    }
  }, GROUP_ID);
  await page.clock.fastForward("02:00");
  await page.evaluate(() => {
    window.dispatchEvent(new Event("offline"));
    window.dispatchEvent(new Event("online"));
  });
}

for (const skin of SKINS) {
  test.describe(`non-admin identity — ${skin} skin`, () => {
    test("a member is offered no rename, for the group or its channel", async ({ page }) => {
      await boot(page, skin, "member");

      await expect(page.getByTestId(`channel-option-${CHANNEL_ID}`)).toBeVisible();
      await expect(page.getByTestId("menu-item-rename-group")).toHaveCount(0);

      await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
      await expect(page.getByTestId("message-input")).toBeVisible();
      await expect(page.getByTestId("rename-channel-trigger")).toHaveCount(0);
    });

    test("an admin still gets the form, with an empty editable description", async ({ page }) => {
      await boot(page, skin, "admin");
      await page.getByTestId("menu-item-rename-group").click();

      await expect(page.getByTestId("rename-group-form")).toBeVisible();
      await expect(page.locator("#rename-group-description")).toBeVisible();
      await expect(page.locator("#rename-group-description")).toHaveValue("");
      await expect(page.getByTestId("rename-group-submit-button")).toBeVisible();
    });

    test("demoted with no description: name as text, no Description, no Save", async ({ page }) => {
      await page.clock.install();
      await boot(page, skin, "admin");
      await page.getByTestId("menu-item-rename-group").click();
      await expect(page.getByTestId("rename-group-form")).toBeVisible();

      await demoteWhileOpen(page);

      await expect(page.getByTestId("rename-group-readonly")).toBeVisible();
      await expect(page.getByTestId("rename-group-name-value")).toHaveText("Book club");
      await expect(page.getByTestId("rename-group-description-value")).toHaveCount(0);
      await expect(page.locator("#rename-group-description")).toHaveCount(0);
      await expect(page.locator("#rename-group-name")).toHaveCount(0);
      await expect(page.getByTestId("rename-group-submit-button")).toHaveCount(0);
      await expect(page.getByText("Description", { exact: true })).toHaveCount(0);
      await expect(page.getByText("Optional description…")).toHaveCount(0);
    });

    test("demoted with a description: it shows as text, not an input", async ({ page }) => {
      await page.clock.install();
      await boot(page, skin, "admin", "We read one book a month.");
      await page.getByTestId("menu-item-rename-group").click();
      await expect(page.getByTestId("rename-group-form")).toBeVisible();

      await demoteWhileOpen(page);

      await expect(page.getByTestId("rename-group-readonly")).toBeVisible();
      await expect(page.getByTestId("rename-group-description-value")).toHaveText(
        "We read one book a month.",
      );
      await expect(page.locator("#rename-group-description")).toHaveCount(0);
      await expect(page.getByTestId("rename-group-submit-button")).toHaveCount(0);
    });

    test("a channel's rename page follows the same rule", async ({ page }) => {
      await page.clock.install();
      await boot(page, skin, "admin");
      await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
      await page.getByTestId("rename-channel-trigger").click();
      await expect(page.getByTestId("rename-channel-form")).toBeVisible();
      await expect(page.locator("#rename-channel-description")).toBeVisible();

      await demoteWhileOpen(page);

      await expect(page.getByTestId("rename-channel-readonly")).toBeVisible();
      await expect(page.getByTestId("rename-channel-name-value")).toHaveText("general");
      await expect(page.getByTestId("rename-channel-description-value")).toHaveCount(0);
      await expect(page.locator("#rename-channel-description")).toHaveCount(0);
      await expect(page.getByTestId("rename-channel-submit-button")).toHaveCount(0);
    });
  });
}
