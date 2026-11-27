// Terminal skin (#2130): a message reads like a shell line. Time, then the name, then the text inline
// after it; wrapped lines continue under the name, not in a hanging block; pictures, files and
// reactions start on their own line under the text, at the name. `TERMINAL_LINE_SHOTS=<dir>`
// writes review screenshots.

import { test, expect, type Locator, type Page } from "./lib/zero-loss";
import { rectOf, type Rect } from "./lib/rects";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_media";
const CHANNEL_ID = "c_general";
const BLURHASH = "LEHV6nWB2yk8pyo0adR*.7kCMdnj";
const SHOTS = process.env.TERMINAL_LINE_SHOTS;

const image = (name: string) => ({
  key: `media/${name}.png`, hash: `h_${name}`, kv: 1, sha: `s_${name}`,
  name: `${name}.png`, ct: "image/png", size: 1024, bh: BLURHASH, w: 400, h: 300,
});
const file = (name: string) => ({
  key: `media/${name}.pdf`, hash: `h_${name}`, kv: 1, sha: `s_${name}`,
  name: `${name}.pdf`, ct: "application/pdf", size: 2048,
});

const msg = (
  id: string,
  senderId: string,
  username: string,
  text: string,
  at: string,
  attachments: unknown[],
  extra: Record<string, unknown> = {},
) => ({
  id,
  conversation_id: CHANNEL_ID,
  sender_id: senderId,
  sender_username: username,
  ciphertext: "",
  content: attachments.length > 0 ? JSON.stringify({ _att: attachments, _txt: text }) : text,
  sent_at: at,
  ...extra,
});

const LONG_TEXT =
  "alright I gotta ship this online state nonsense, the presence dots have been lying to us all " +
  "week and I am done pretending the reconnect path is fine. Plan: drop the heartbeat, trust the " +
  "socket close, and show the last-seen time instead of a green dot when we are not sure. Shout " +
  "if that breaks anything you rely on before I merge it tonight.";
const ARABIC_LONG =
  "ملاحظات الشحن لمكتب لندن ".repeat(14).trim();

const MESSAGES = [
  msg("m_long", "u_dana", "dana", LONG_TEXT, "2026-08-01T09:00:00Z", []),
  msg("m_photo", "u_dana", "dana", "here is the map", "2026-08-01T09:00:20Z", [image("map")]),
  msg("m_only", "u_dana", "dana", "", "2026-08-01T09:00:40Z", [image("solo")]),
  msg("m_multi", "u_dana", "dana", "holiday photos", "2026-08-01T09:01:00Z", [image("a"), image("b"), image("c"), file("itinerary")]),
  // A reply from someone with a longer name, so the text starts further along the line.
  msg("m_reply", "u_bartholomew", "bartholomew", "mine", "2026-08-01T09:02:00Z", [image("e")], { reply_to_id: "m_multi" }),
  msg("m_arabic", "u_bartholomew", "bartholomew", ARABIC_LONG, "2026-08-01T09:03:00Z", []),
];

const REACTIONS = {
  m_long: [{ emoji: "👍", user_ids: ["u_bartholomew"], count: 1 }],
  m_multi: [{ emoji: "👍", user_ids: ["u_bartholomew"], count: 1 }],
  m_reply: [{ emoji: "❤️", user_ids: ["u_dana"], count: 1 }],
};

async function openChannel(page: Page, lang = "en") {
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
      groups: [
        { id: GROUP_ID, name: "media", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
      ],
      channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
      groupMembers: {
        [GROUP_ID]: [
          { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
          { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
          { user_id: "u_bartholomew", username: "bartholomew", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        ],
      },
      messages: { [CHANNEL_ID]: MESSAGES },
      reactions: REACTIONS,
      dmChannels: [],
      preferences: { skin: "terminal" },
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.locator("html")).toHaveAttribute("dir", lang === "ar" ? "rtl" : "ltr");
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-m_arabic").getByTestId("message-content")).toBeVisible();
}

/** One rect per painted line of an inline element (`getClientRects`), top to bottom. */
const lineRects = (locator: Locator): Promise<Rect[]> =>
  locator.evaluate((el) => {
    const byLine = new Map<number, { left: number; right: number; top: number; bottom: number }>();
    for (const r of Array.from(el.getClientRects())) {
      if (r.width === 0) {
        continue;
      }
      const key = Math.round(r.top);
      const prev = byLine.get(key);
      byLine.set(key, prev
        ? { left: Math.min(prev.left, r.left), right: Math.max(prev.right, r.right), top: prev.top, bottom: Math.max(prev.bottom, r.bottom) }
        : { left: r.left, right: r.right, top: r.top, bottom: r.bottom });
    }
    return [...byLine.values()].sort((a, b) => a.top - b.top);
  });

const near = (a: number, b: number, slack = 1.5) => Math.abs(a - b) <= slack;

test.describe("terminal skin — a message reads like a shell line", () => {
  test("a long message starts after the name and wraps back under the name", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_long");
    const time = await rectOf(row.getByTestId("message-timestamp"));
    const author = await rectOf(row.getByTestId("message-author"));
    const lines = await lineRects(row.getByTestId("message-content"));

    expect(author.left).toBeGreaterThan(time.right - 1);
    expect(lines.length).toBeGreaterThan(1);
    // First line: inline with the name, after it.
    expect(lines[0].left).toBeGreaterThan(author.right);
    expect(lines[0].top).toBeLessThan(author.bottom);
    // Every wrapped line starts where the name starts: no hanging indent.
    for (const line of lines.slice(1)) {
      expect(near(line.left, author.left), `line at ${line.left}, name at ${author.left}`).toBe(true);
    }

    const reactions = await rectOf(row.getByTestId("message-reactions"));
    expect(near(reactions.left, author.left)).toBe(true);
    expect(reactions.top).toBeGreaterThanOrEqual(lines[lines.length - 1].bottom - 1);

    if (SHOTS) {
      await row.screenshot({ path: `${SHOTS}/terminal-long-wrapped.png` });
    }
  });

  for (const id of ["m_photo", "m_multi", "m_reply"]) {
    test(`${id}: text first, then the pictures on their own line at the name`, async ({ page }) => {
      await openChannel(page);
      const row = page.getByTestId(`message-${id}`);
      const author = await rectOf(row.getByTestId("message-author"));
      const content = await rectOf(row.getByTestId("message-content"));
      expect(content.left).toBeGreaterThan(author.right - 1);
      expect(content.top).toBeLessThan(author.bottom);

      const thumbs = row.locator('[data-testid^="attachment-media/"][data-testid$=".png"]');
      expect(await thumbs.count()).toBeGreaterThan(0);
      const thumb = await rectOf(thumbs.first());
      expect(near(thumb.left, author.left)).toBe(true);
      expect(thumb.top).toBeGreaterThanOrEqual(content.bottom - 1);
      expect(thumb.top).toBeGreaterThanOrEqual(author.bottom - 1);

      if (SHOTS && id === "m_photo") {
        await row.screenshot({ path: `${SHOTS}/terminal-text-and-image.png` });
      }
    });
  }

  test("a picture with no text still starts on its own line, under the name", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_only");
    const author = await rectOf(row.getByTestId("message-author"));
    const thumb = await rectOf(row.locator('[data-testid="attachment-media/solo.png"]'));
    expect(near(thumb.left, author.left)).toBe(true);
    expect(thumb.top).toBeGreaterThanOrEqual(author.bottom - 1);
    // No ":" with nothing after it.
    const line = await row.getByTestId("message-author").evaluate((el) => el.parentElement!.textContent);
    expect(line?.trim()).toBe("dana");
  });

  test("a file row in a multi-attachment message starts at the name too", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_multi");
    const author = await rectOf(row.getByTestId("message-author"));
    await expect(row.locator('[data-testid^="attachment-media/"][data-testid$=".png"]')).toHaveCount(3);
    const fileRow = await rectOf(row.getByTestId("attachment-media/itinerary.pdf"));
    expect(near(fileRow.left, author.left)).toBe(true);
    const reactions = await rectOf(row.getByTestId("message-reactions"));
    expect(near(reactions.left, author.left)).toBe(true);
  });

  test("the reply preview stays above the line", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_reply");
    const preview = await rectOf(row.getByTestId("reply-preview-m_multi"));
    const author = await rectOf(row.getByTestId("message-author"));
    expect(preview.bottom).toBeLessThanOrEqual(author.top + 1);
  });

  test("the hover actions float over the row's top edge, clear of the text", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_long");
    await row.hover();
    const actions = await rectOf(row.getByTestId("message-actions"));
    const author = await rectOf(row.getByTestId("message-author"));
    const lines = await lineRects(row.getByTestId("message-content"));
    // Above the line, at the row's end (#2144): the text keeps the full width under them.
    expect(actions.bottom).toBeLessThanOrEqual(Math.min(author.top, lines[0].top) + 0.5);
    expect(lines[0].right).toBeGreaterThan(actions.left);
  });

  test("selecting the line copies it as name: text", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_photo");
    const text = await row.evaluate((el) => {
      const range = document.createRange();
      range.selectNodeContents(el.querySelector('[data-testid="message-content"]')!.parentElement!);
      const sel = window.getSelection()!;
      sel.removeAllRanges();
      sel.addRange(range);
      return sel.toString();
    });
    expect(text.replace(/\s+/g, " ").trim()).toBe("dana: here is the map");
  });

  test("right to left: the line mirrors and wraps back under the name", async ({ page }) => {
    await openChannel(page, "ar");
    for (const id of ["m_long", "m_arabic"]) {
      const row = page.getByTestId(`message-${id}`);
      const time = await rectOf(row.getByTestId("message-timestamp"));
      const author = await rectOf(row.getByTestId("message-author"));
      const lines = await lineRects(row.getByTestId("message-content"));
      expect(author.right).toBeLessThan(time.left + 1);
      expect(lines.length).toBeGreaterThan(1);
      expect(lines[0].right).toBeLessThan(author.left);
      for (const line of lines.slice(1)) {
        expect(near(line.right, author.right), `${id}: line ends ${line.right}, name ends ${author.right}`).toBe(true);
      }
    }
    if (SHOTS) {
      await page.getByTestId("message-m_arabic").screenshot({ path: `${SHOTS}/terminal-rtl-wrapped.png` });
    }
  });
});
