// Long handles (#1971): Members, Message requests and the DM lists hold a 40-character handle
// on one line without pushing their controls out of reach or the page sideways.

import { test, expect, type Locator, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_crew";
const CHANNEL_ID = "c_general";
// Longer than any real handle (the DS caps them at 32): the layout must not care.
const LONG_MEMBER = "lorem.ipsum.dolor.sit.amet.consectetur1";
const LONG_DM = "sed.do.eiusmod.tempor.incididunt.labore2";
const LONG_REQUEST = "ut.enim.ad.minim.veniam.quis.nostrud.ex3";
const SHOTS = process.env.POLLIS_E2E_SHOTS;

const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

const member = (user_id: string, username: string, role: string) => ({
  user_id,
  username,
  role,
  joined_at: "2026-01-01T00:00:00Z",
});

const dm = (id: string, peerId: string, peer: string) => ({
  id,
  created_by: peerId,
  created_at: "2026-08-01T09:00:00Z",
  members: [
    { user_id: ME.id, username: ME.username, added_by: peerId, added_at: "2026-08-01T09:00:00Z" },
    { user_id: peerId, username: peer, added_by: peerId, added_at: "2026-08-01T09:00:00Z" },
  ],
});

async function boot(page: Page, skin: Skin) {
  test.info().annotations.push({ type: "skin", description: skin });
  await page.addInitScript(
    (data) => {
      (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
    },
    {
      session: ME,
      profile: { id: ME.id, username: ME.username },
      groups: [
        { id: GROUP_ID, name: "crew", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
      ],
      channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
      groupMembers: {
        [GROUP_ID]: [member(ME.id, ME.username, "admin"), member("u_long", LONG_MEMBER, "member")],
      },
      dmChannels: [dm("dm_long", "u_dm", LONG_DM)],
      dmRequests: [dm("dm_req", "u_req", LONG_REQUEST)],
      messages: {},
      preferences: { skin },
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

/** The row keeps its content inside itself, and the page never scrolls sideways. */
async function expectContained(page: Page, row: Locator) {
  await expect(row).toBeVisible();
  const overflow = await row.evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(overflow, "the row's content overflows it").toBeLessThanOrEqual(1);
  const pageOverflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  expect(pageOverflow, "the page scrolls sideways").toBeLessThanOrEqual(0);
}

/** A control is on screen and fully inside the viewport. */
async function expectReachable(page: Page, control: Locator) {
  await expect(control).toBeVisible();
  const box = await control.boundingBox();
  const width = page.viewportSize()?.width ?? 0;
  expect(box, "control has no box").not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(width);
}

async function shoot(page: Page, name: string) {
  if (SHOTS) {
    await page.screenshot({ path: `${SHOTS}/desktop-${name}.png` });
  }
}

for (const skin of SKINS) {
  for (const width of [1280, 900]) {
    test(`${skin} ${width}px: members, requests and DM lists hold a 40-character handle`, async ({ page }) => {
      await page.setViewportSize({ width, height: 800 });
      await boot(page, skin);

      // The sidebar DM row truncates the handle inside the sidebar.
      const sidebarDm = page.getByTestId("sidebar-row-dm-dm_long");
      await expectContained(page, sidebarDm);

      // Members: the admin's role switch and Kick stay on screen beside the long name.
      await page.getByTestId("sidebar").getByRole("button", { name: "crew", exact: true }).click();
      await page.getByTestId("menu-item-members").click();
      const memberRow = page.getByTestId("member-row-u_long");
      await expectContained(page, memberRow);
      await expectReachable(page, page.getByTestId("member-kick-u_long"));
      await shoot(page, `${skin}-${width}-members`);

      // The DMs hub lists the conversation and links the request.
      await page.getByTestId("sidebar-row-dms").click();
      await expectContained(page, page.getByTestId("dm-option-dm_long"));
      await shoot(page, `${skin}-${width}-dms`);

      // Message requests: Accept and Block stay on screen.
      await page.getByTestId("menu-item-dm-requests").click();
      const requestRow = page.getByTestId("request-dm_req");
      await expectContained(page, requestRow);
      await expect(requestRow).toContainText(LONG_REQUEST.slice(0, 12));
      await expectReachable(page, page.getByTestId("accept-request-dm_req"));
      await expectReachable(page, page.getByTestId("block-request-dm_req"));
      await shoot(page, `${skin}-${width}-requests`);
    });
  }
}
