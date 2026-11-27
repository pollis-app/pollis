// Names in the chat (#2146). My own name has one colour everywhere the chat shows it: my per-user
// colour, the same rule as everyone else's (no accent for self, which #1488 had), in the message
// line, the reply quote on a row, the reply bar above the composer, the thread view and the pinned
// list, in both looks. In the Terminal look a reply quote's arrow starts where the names start,
// left to right and right to left. `NAME_COLOUR_SHOTS=<dir>` writes review screenshots.

import { test, expect, type Locator, type Page } from "./lib/zero-loss";
import { rectOf } from "./lib/rects";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_names";
const CHANNEL_ID = "c_general";
const SHOTS = process.env.NAME_COLOUR_SHOTS;

const msg = (id: string, senderId: string, username: string, content: string, minute: number, extra = {}) => ({
  id,
  conversation_id: CHANNEL_ID,
  sender_id: senderId,
  sender_username: username,
  ciphertext: "",
  content,
  sent_at: new Date(Date.UTC(2026, 7, 1, 9, minute)).toISOString(),
  ...extra,
});

const MESSAGES = [
  msg("m_mine", ME.id, "mia", "shipping the presence fix tonight", 0),
  msg("m_dana", "u_dana", "dana", "sounds good", 1),
  // Dana quotes me, then I quote Dana: my name in a quote, and a quote on my own row.
  msg("m_quote_me", "u_dana", "dana", "wait, tonight?", 2, { reply_to_id: "m_mine" }),
  msg("m_quote_her", ME.id, "mia", "yes, after the build", 3, { reply_to_id: "m_dana" }),
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

const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

async function openChannel(page: Page, skin: Skin, lang = "en") {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.addInitScript(({ data, lang }) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
    Object.defineProperty(navigator, "languages", { get: () => [lang], configurable: true });
    Object.defineProperty(navigator, "language", { get: () => lang, configurable: true });
  }, {
    lang,
    data: {
      session: ME,
      profile: { id: ME.id, username: ME.username },
      // A member, not an admin: the plain coloured name. The admin chip is admin-name-chips.spec.
      groups: [
        { id: GROUP_ID, name: "crew", owner_id: "u_dana", created_at: "2026-01-01T00:00:00Z", current_user_role: "member" },
      ],
      channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
      groupMembers: {
        [GROUP_ID]: [
          { user_id: ME.id, username: "mia", role: "member", joined_at: "2026-01-01T00:00:00Z" },
          { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        ],
      },
      messages: { [CHANNEL_ID]: MESSAGES },
      pins: [],
      dmChannels: [],
      preferences: { skin },
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.locator("html")).toHaveAttribute("dir", lang === "ar" ? "rtl" : "ltr");
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-m_quote_her").getByTestId("message-content")).toBeVisible();
}

const resolve = (page: Page, css: string) =>
  page.evaluate((value) => {
    const probe = document.createElement("div");
    probe.style.color = value;
    document.body.appendChild(probe);
    const out = getComputedStyle(probe).color;
    probe.remove();
    return out;
  }, css);

/** The colour a name paints in: its own colour, with any opacity on the way up multiplied in. */
const paintOf = (locator: Locator) =>
  locator.evaluate((el) => {
    let opacity = 1;
    for (let n: Element | null = el; n; n = n.parentElement) {
      opacity *= Number(getComputedStyle(n).opacity);
    }
    return { color: getComputedStyle(el).color, opacity };
  });

const row = (page: Page, id: string) => page.getByTestId(`message-${id}`);
const quoteName = (page: Page, id: string, quoted: string) =>
  row(page, id).getByTestId(`reply-preview-${quoted}`).locator("bdi");

for (const skin of SKINS) {
  test.describe(`names in the chat — ${skin} look`, () => {
    test("my name has my one per-user colour everywhere the chat shows it", async ({ page }) => {
      await openChannel(page, skin);
      const mine = await resolve(page, expectedColor("mia", false));
      const accent = await resolve(page, "var(--c-accent)");
      expect(mine).not.toBe(accent);

      const expectMine = async (where: string, locator: Locator) => {
        const paint = await paintOf(locator);
        expect(paint.color, where).toBe(mine);
        expect(paint.opacity, `${where}: not faded`).toBe(1);
      };

      // The message line, on both of my rows.
      await expectMine("message line", row(page, "m_mine").getByTestId("message-author"));
      await expectMine("message line (reply)", row(page, "m_quote_her").getByTestId("message-author"));
      // Dana's row quoting me.
      await expectMine("reply quote", quoteName(page, "m_quote_me", "m_mine"));
      // Others keep theirs, the same rule.
      const dana = await resolve(page, expectedColor("dana", false));
      expect((await paintOf(row(page, "m_dana").getByTestId("message-author"))).color).toBe(dana);
      expect((await paintOf(quoteName(page, "m_quote_her", "m_dana"))).color).toBe(dana);

      // The reply bar above the composer, replying to myself.
      await row(page, "m_mine").hover();
      await row(page, "m_mine").getByTestId("reply-button").click();
      await expectMine("reply bar", page.getByTestId("reply-preview").locator("span[style]").first());
      await page.keyboard.press("Escape");

      // The thread view.
      await row(page, "m_mine").hover();
      await row(page, "m_mine").getByTestId("message-actions-more").click();
      await page.getByTestId("thread-button").click();
      await expectMine("thread", page.getByTestId("thread-root").getByTestId("thread-author"));

      // The pinned list.
      await row(page, "m_mine").hover();
      await row(page, "m_mine").getByTestId("message-actions-more").click();
      await page.getByTestId("pin-button").click();
      await page.getByTestId("channel-pins-trigger").click();
      await expect(page.getByTestId("pinned-panel-row")).toHaveCount(1);
      await expectMine("pinned list", page.getByTestId("pinned-panel-author"));

      if (SHOTS) {
        await page.screenshot({ path: `${SHOTS}/${skin}-names.png` });
      }
    });
  });
}

test.describe("terminal look — the reply quote lines up with the names", () => {
  for (const lang of ["en", "ar"]) {
    test(`${lang === "ar" ? "right to left" : "left to right"}: the arrow starts where the names start`, async ({ page }) => {
      await openChannel(page, "terminal", lang);
      const rtl = lang === "ar";
      for (const [id, quoted] of [["m_quote_me", "m_mine"], ["m_quote_her", "m_dana"]]) {
        const arrow = await rectOf(row(page, id).getByTestId(`reply-preview-${quoted}`).locator("svg").first());
        const name = await rectOf(row(page, id).getByTestId("message-author"));
        const edge = rtl ? [arrow.right, name.right] : [arrow.left, name.left];
        expect(Math.abs(edge[0] - edge[1]), `${id}: arrow ${edge[0]}, name ${edge[1]}`).toBeLessThanOrEqual(1);
        // And every other name in the log starts on the same line.
        const other = await rectOf(row(page, "m_dana").getByTestId("message-author"));
        expect(Math.abs((rtl ? other.right : other.left) - edge[1])).toBeLessThanOrEqual(1);
      }
      if (SHOTS) {
        await page.getByTestId("message-list").screenshot({ path: `${SHOTS}/terminal-reply-${lang}.png` });
      }
    });
  }
});
