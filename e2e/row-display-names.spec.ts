// Display names arrive with the rows that list people (#2085), desktop: the member page names a
// member by display name with the handle beside it on its first render, and an invitee by
// display name, with no profile read behind either.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP = "g_team";

const PRELOAD = {
  session: ME,
  profile: { id: ME.id, username: ME.username },
  groups: [{ id: GROUP, name: "Team", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" }],
  channels: { [GROUP]: [{ id: "c_general", group_id: GROUP, name: "general" }] },
  groupMembers: {
    [GROUP]: [
      { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      { user_id: "u_bob", username: "bob", display_name: "Bob Builder", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      { user_id: "u_cat", username: "cat", role: "member", joined_at: "2026-01-01T00:00:00Z" },
    ],
  },
  groupInvites: [
    {
      id: "inv_1",
      group_id: GROUP,
      invitee_id: "u_dan",
      invitee_username: "dan",
      invitee_preferred_name: "Dan Invitee",
      inviter_id: ME.id,
      inviter_username: "mia",
      created_at: "2026-01-02T00:00:00Z",
    },
  ],
};

async function openMembers(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.evaluate(() => {
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
  });
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP}`).click();
  await page.getByTestId("menu-item-members").click();
  await expect(page.getByTestId("members-list")).toBeVisible();
}

function count(page: Page, command: string): Promise<number> {
  return page.evaluate(
    (cmd) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[cmd] ?? 0,
    command,
  );
}

test("a member shows by display name, handle beside it; one without shows the handle alone", async ({ page }) => {
  await openMembers(page);
  await expect(page.getByTestId("member-name-u_bob")).toHaveText("Bob Builder");
  await expect(page.getByTestId("member-handle-u_bob")).toHaveText("@bob");
  await expect(page.getByTestId("member-name-u_cat")).toHaveText("cat");
  await expect(page.getByTestId("member-handle-u_cat")).toHaveCount(0);
  expect(await count(page, "get_user_profiles")).toBe(0);
});

test("an invitee shows by display name", async ({ page }) => {
  await openMembers(page);
  await expect(page.getByTestId("invited-row-u_dan")).toContainText("Dan Invitee");
});
