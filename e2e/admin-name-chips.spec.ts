// Admin name chips (#1488): every admin's chip, your own included (#2146), is their per-user colour.
// A DM has no admins (#2126): no chip there, even with an admin's group still selected behind it.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_admins";
const CHANNEL_ID = "c_general";
const DM_ID = "dm_dana";

const msg = (id: string, senderId: string, username: string, text: string, at: string, conversation = CHANNEL_ID) => ({
  id,
  conversation_id: conversation,
  sender_id: senderId,
  sender_username: username,
  ciphertext: "",
  content: text,
  sent_at: at,
});

const MESSAGES = [
  msg("m_mia", ME.id, "mia", "my own admin name", "2026-08-01T09:00:00Z"),
  msg("m_dana", "u_dana", "dana", "an admin", "2026-08-01T09:01:00Z"),
  msg("m_eve", "u_eve", "eve", "another admin", "2026-08-01T09:02:00Z"),
  msg("m_zed", "u_zed", "zed", "just a member", "2026-08-01T09:03:00Z"),
];

// Mirror of getUsernameColor in frontend/src/utils/usernameColor.ts.
function expectedColor(key: string, isLightBg: boolean): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return `hsl(${(h >>> 0) % 360} 65% ${isLightBg ? 35 : 68}%)`;
}

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

async function openChannel(page: Page, skin: Skin, background?: string) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "crew", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_eve", username: "eve", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_zed", username: "zed", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {
      [CHANNEL_ID]: MESSAGES,
      [DM_ID]: [
        msg("dm_mia", ME.id, "mia", "my name in a DM", "2026-08-01T10:00:00Z", DM_ID),
        msg("dm_dana", "u_dana", "dana", "her name in a DM", "2026-08-01T10:01:00Z", DM_ID),
      ],
    },
    dmChannels: [
      {
        id: DM_ID,
        created_by: "u_dana",
        created_at: "2026-01-01T00:00:00Z",
        members: [
          { user_id: ME.id, username: "mia" },
          { user_id: "u_dana", username: "dana" },
        ],
      },
    ],
    preferences: { skin, ...(background ? { background_color: background } : {}) },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-m_zed").getByTestId("message-content")).toBeVisible();
}

const author = (page: Page, id: string) =>
  page.getByTestId(`message-${id}`).getByTestId("message-author");

const paintOf = (page: Page, id: string) =>
  author(page, id).evaluate((el) => {
    const s = getComputedStyle(el);
    return { background: s.backgroundColor, color: s.color };
  });

const resolve = (page: Page, css: string) =>
  page.evaluate((value) => {
    const probe = document.createElement("div");
    probe.style.color = value;
    document.body.appendChild(probe);
    const out = getComputedStyle(probe).color;
    probe.remove();
    return out;
  }, css);

function isDark(rgb: string): boolean {
  const [r, g, b] = rgb.match(/\d+/g)!.map(Number);
  return r < 40 && g < 40 && b < 40;
}

const TRANSPARENT = "rgba(0, 0, 0, 0)";

for (const skin of SKINS) {
  test.describe(`admin name chips — ${skin} skin`, () => {
    test("every admin's chip, my own included, is their own colour under dark ink", async ({ page }) => {
      await openChannel(page, skin);
      const accent = await resolve(page, "var(--c-accent)");

      // No accent for self (#2146): my chip follows the rule everyone else's does.
      const mine = await paintOf(page, "m_mia");
      expect(mine.background).toBe(await resolve(page, expectedColor("mia", false)));
      expect(mine.background).not.toBe(accent);
      expect(isDark(mine.color)).toBe(true);

      const dana = await paintOf(page, "m_dana");
      const eve = await paintOf(page, "m_eve");
      expect(dana.background).toBe(await resolve(page, expectedColor("dana", false)));
      expect(eve.background).toBe(await resolve(page, expectedColor("eve", false)));
      expect(dana.background).not.toBe(eve.background);
      expect(dana.background).not.toBe(accent);
      expect(isDark(dana.color)).toBe(true);
      expect(isDark(eve.color)).toBe(true);
    });

    test("a non-admin name is unchanged: coloured text, no fill", async ({ page }) => {
      await openChannel(page, skin);
      const zed = await paintOf(page, "m_zed");
      expect(zed.background).toBe(TRANSPARENT);
      expect(zed.color).toBe(await resolve(page, expectedColor("zed", false)));
    });

    test("a DM has no admins: plain names after leaving an admin's group for the DM (#2126)", async ({ page }) => {
      await openChannel(page, skin);
      // The group's chips are up, so the group really is selected behind the DM.
      expect((await paintOf(page, "m_mia")).background).not.toBe(TRANSPARENT);

      // The sidebar row: the way Dan got there, with the group still selected.
      await page.getByTestId(`sidebar-row-dm-${DM_ID}`).click();
      await expect(page.getByTestId("message-dm_dana").getByTestId("message-content")).toBeVisible();

      for (const id of ["dm_mia", "dm_dana"]) {
        expect((await paintOf(page, id)).background).toBe(TRANSPARENT);
      }
      await page.screenshot({ path: `artifacts/admin-name-chips/${skin}-dm.png` });
    });

    test("on a light background the chip keeps the bright fill and dark ink", async ({ page }) => {
      await openChannel(page, skin, "#f4f1ea");
      const dana = await paintOf(page, "m_dana");
      expect(dana.background).toBe(await resolve(page, expectedColor("dana", false)));
      expect(isDark(dana.color)).toBe(true);
      // A member's text still follows the light-theme variant.
      const zed = await paintOf(page, "m_zed");
      expect(zed.color).toBe(await resolve(page, expectedColor("zed", true)));
    });
  });
}
