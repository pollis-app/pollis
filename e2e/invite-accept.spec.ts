// Accepting a group invite is optimistic (#1560); a failed accept rolls back.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const INVITE_ID = "inv_crew";
const GROUP_ID = "g_crew";
const GENERAL = "c_general";

// Every read the accept invalidates. Holding them proves the group row on
// screen is the optimistic one, not a refetch that happened to be fast.
const REFETCHES = ["get_pending_invites", "list_user_groups", "list_user_groups_with_channels"];

async function boot(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [],
    channels: {},
    pendingInvites: [
      {
        id: INVITE_ID,
        group_id: GROUP_ID,
        group_name: "crew",
        inviter_id: "u_dana",
        inviter_username: "dana",
        created_at: "2026-10-01T09:00:00Z",
        channels: [{ id: GENERAL, group_id: GROUP_ID, name: "general" }],
      },
    ],
    messages: { [GENERAL]: [] },
    dmChannels: [],
    preferences: { skin: "terminal" },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-invites").click();
  await expect(page.getByTestId(`invite-${INVITE_ID}`)).toBeVisible();
}

const sidebarGroup = (page: Page) =>
  page.getByTestId("sidebar").getByRole("button", { name: "crew", exact: true });

async function hold(page: Page, commands: string[]) {
  await page.evaluate((cmds) => {
    const w = window as unknown as { __tauriHold: (cmd: string) => void };
    cmds.forEach((c) => w.__tauriHold(c));
  }, commands);
}

async function release(page: Page, commands: string[]) {
  await page.evaluate((cmds) => {
    const w = window as unknown as { __tauriRelease: (cmd: string) => void };
    cmds.forEach((c) => w.__tauriRelease(c));
  }, commands);
}

test("accepting shows the group at once, then it opens", async ({ page }) => {
  await boot(page);
  await expect(sidebarGroup(page)).toHaveCount(0);

  await hold(page, REFETCHES);
  await page.getByTestId(`accept-invite-${INVITE_ID}`).click();

  // Optimistic: the refetches are all still held.
  await expect(page.getByTestId(`invite-${INVITE_ID}`)).toHaveCount(0);
  await expect(sidebarGroup(page)).toBeVisible();
  const asked = await page.evaluate(() => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts);
  // The welcome is still pulled on the accept path.
  expect(asked.poll_mls_welcomes ?? 0).toBeGreaterThan(0);

  await release(page, REFETCHES);
  await sidebarGroup(page).click();
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(page.getByRole("textbox", { name: "Message input" })).toBeVisible();
});

test("a failed accept puts the invite back and says so", async ({ page }) => {
  await boot(page);
  await page.evaluate(() => {
    (window as unknown as { __tauriRejectWith: (cmd: string, msg: string) => void }).__tauriRejectWith(
      "accept_group_invite",
      "network down",
    );
  });

  await page.getByTestId(`accept-invite-${INVITE_ID}`).click();

  await expect(page.getByTestId("accept-invite-error")).toHaveText("Couldn't accept the invite. Try again.");
  await expect(page.getByTestId(`invite-${INVITE_ID}`)).toBeVisible();
  await expect(sidebarGroup(page)).toHaveCount(0);
});
