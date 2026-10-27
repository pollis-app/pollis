/*
 * Group membership without timeline lines (#1506).
 *
 *   - No "X joined / left / added a device" line ever renders in a channel,
 *     whatever a roster_changed event carries.
 *   - A member's messages carry a small "new" tag for 7 days after their
 *     server-side `joined_at`, in every channel of the group. The group's
 *     creator never carries it.
 *   - The member page lists pending invites under "Invited" (invitee, who
 *     invited, when); the inviter and admins can revoke.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true` (see
 * playwright.config.js).
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_crew";
const GENERAL = "c_general";
const RANDOM = "c_random";

const msg = (channel: string, id: string, senderId: string, username: string, at: string) => ({
  id,
  conversation_id: channel,
  sender_id: senderId,
  sender_username: username,
  ciphertext: "",
  content: `hello from ${username}`,
  sent_at: at,
});

// mia created the group; dana joined (accepted) on Aug 1 at 09:00 UTC, in the
// DS's own zone-less format; eve has been around for months; zed joined Aug 1
// too but posts eight days later.
const MEMBERS = (myRole: "admin" | "member") => [
  { user_id: ME.id, username: "mia", role: myRole, joined_at: "2026-01-01 00:00:00" },
  { user_id: "u_dana", username: "dana", role: "admin", joined_at: "2026-08-01 09:00:00" },
  { user_id: "u_eve", username: "eve", role: "member", joined_at: "2026-03-01 09:00:00" },
  { user_id: "u_zed", username: "zed", role: "member", joined_at: "2026-08-01 09:00:00" },
];

const INVITES = [
  {
    id: "inv_by_me",
    group_id: GROUP_ID,
    invitee_id: "u_ola",
    invitee_username: "ola",
    inviter_id: ME.id,
    inviter_username: "mia",
    created_at: "2026-08-02 10:00:00",
  },
  {
    id: "inv_by_dana",
    group_id: GROUP_ID,
    invitee_id: "u_pip",
    invitee_username: "pip",
    inviter_id: "u_dana",
    inviter_username: "dana",
    created_at: "2026-08-03 10:00:00",
  },
];

async function boot(page: Page, opts: { skin: "terminal" | "refined"; myRole: "admin" | "member" }) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "crew",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: opts.myRole,
      },
    ],
    channels: {
      [GROUP_ID]: [
        { id: GENERAL, group_id: GROUP_ID, name: "general" },
        { id: RANDOM, group_id: GROUP_ID, name: "random" },
      ],
    },
    groupMembers: { [GROUP_ID]: MEMBERS(opts.myRole) },
    groupInvites: INVITES,
    messages: {
      [GENERAL]: [
        msg(GENERAL, "m_mia", ME.id, "mia", "2026-08-01T09:05:00Z"),
        msg(GENERAL, "m_dana", "u_dana", "dana", "2026-08-03T12:00:00Z"),
        msg(GENERAL, "m_eve", "u_eve", "eve", "2026-08-03T12:01:00Z"),
        msg(GENERAL, "m_zed", "u_zed", "zed", "2026-08-09T12:00:00Z"),
      ],
      [RANDOM]: [msg(RANDOM, "m_dana_random", "u_dana", "dana", "2026-08-07T08:00:00Z")],
    },
    dmChannels: [],
    preferences: { skin: opts.skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openChannel(page: Page, channelId: string, lastMessageId: string) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${channelId}`).click();
  await expect(page.getByTestId(`message-${lastMessageId}`).getByTestId("message-content")).toBeVisible();
}

const newTag = (page: Page, messageId: string) =>
  page.getByTestId(`message-${messageId}`).getByTestId("message-new-tag");

for (const skin of ["terminal", "refined"] as const) {
  test.describe(`new member tag — ${skin} skin`, () => {
    test("tags a member's messages for 7 days after joining, never the creator", async ({ page }) => {
      await boot(page, { skin, myRole: "admin" });
      await openChannel(page, GENERAL, "m_zed");

      await expect(newTag(page, "m_dana")).toHaveText("new");
      // The creator joined with the group: never tagged.
      await expect(newTag(page, "m_mia")).toHaveCount(0);
      // Joined months ago.
      await expect(newTag(page, "m_eve")).toHaveCount(0);
      // Joined Aug 1, posted Aug 9: past the 7-day window.
      await expect(newTag(page, "m_zed")).toHaveCount(0);
    });
  });
}

test("the tag follows the member into every channel of the group", async ({ page }) => {
  await boot(page, { skin: "refined", myRole: "admin" });
  await openChannel(page, RANDOM, "m_dana_random");
  await expect(newTag(page, "m_dana_random")).toHaveText("new");
});

test("a roster change never puts a membership line in the timeline", async ({ page }) => {
  await boot(page, { skin: "refined", myRole: "admin" });
  await openChannel(page, GENERAL, "m_zed");

  // Even a payload in the old, id-carrying shape renders nothing.
  await page.evaluate((groupId) => {
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
      type: "roster_changed",
      conversation_id: groupId,
      epoch_before: 3,
      epoch_after: 4,
      joined_user_ids: ["u_ola"],
      left_user_ids: ["u_eve"],
      devices_added: [["u_dana", "d_new"]],
      devices_removed: [["u_zed", "d_old"]],
    });
  }, GROUP_ID);

  const log = page.getByTestId("message-list");
  await expect(log.getByTestId("message-m_zed")).toBeVisible();
  await expect(page.locator('[data-testid^="roster-banner-"]')).toHaveCount(0);
  await expect(log).not.toContainText(/joined the group|left the group|device/);
});

async function gotoMembers(page: Page) {
  await page.getByTestId("sidebar").getByRole("button", { name: "crew", exact: true }).click();
  await page.getByTestId("menu-item-members").click();
  await expect(page.getByTestId("members-list")).toBeVisible();
}

test("an admin sees every pending invite and can revoke any of them", async ({ page }) => {
  await boot(page, { skin: "terminal", myRole: "admin" });
  await gotoMembers(page);

  const section = page.getByTestId("invited-section");
  await expect(section).toContainText("Invited");
  const ola = page.getByTestId("invited-row-u_ola");
  const pip = page.getByTestId("invited-row-u_pip");
  await expect(ola).toContainText("ola");
  await expect(ola).toContainText("invited by mia");
  await expect(pip).toContainText("invited by dana");
  // Pending invitees are not members.
  await expect(page.getByTestId("member-row-u_ola")).toHaveCount(0);

  await page.getByTestId("invited-revoke-u_pip").click();
  await expect(pip).toHaveCount(0);
  await expect(ola).toBeVisible();

  await page.getByTestId("invited-revoke-u_ola").click();
  // The section goes away with its last invite.
  await expect(section).toHaveCount(0);
});

test("a plain member can revoke only the invites they sent", async ({ page }) => {
  await boot(page, { skin: "terminal", myRole: "member" });
  await gotoMembers(page);

  await expect(page.getByTestId("invited-row-u_ola")).toBeVisible();
  await expect(page.getByTestId("invited-row-u_pip")).toBeVisible();
  await expect(page.getByTestId("invited-revoke-u_ola")).toBeVisible();
  await expect(page.getByTestId("invited-revoke-u_pip")).toHaveCount(0);
});
