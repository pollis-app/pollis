// The hover actions never cover a message's text (#2144). Slack/Discord style: they float at the
// row's top-end corner over its top edge, so the first line stays readable, takes the pointer and
// selects on double-click while they show. Both looks, wide and narrow columns, left to right and
// right to left, the list's first row, and the keyboard's focus-within reveal.
// `MESSAGE_ACTIONS_SHOTS=<dir>` writes review screenshots.

import { test, expect, type Locator, type Page } from "./lib/zero-loss";
import { intersects, rectOf, type Rect } from "./lib/rects";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_actions";
const CHANNEL_ID = "c_general";
const SHOTS = process.env.MESSAGE_ACTIONS_SHOTS;

const LONG =
  "alright I gotta ship this online state change, the presence dots have been lying to us all " +
  "week and I am done pretending the reconnect path is fine, so shout before I merge it tonight";
const ARABIC = "ملاحظات الشحن لمكتب لندن ".repeat(8).trim();

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

// A group start, continuations, someone else's start, a reply and a right-to-left body.
const MESSAGES = [
  msg("m_first", "u_dana", "dana", LONG, 0),
  msg("m_cont", "u_dana", "dana", `and one more thing: ${LONG}`, 1),
  msg("m_short", "u_dana", "dana", "ok", 2),
  msg("m_other", "u_bart", "bartholomew", LONG, 3),
  msg("m_reply", "u_dana", "dana", `replying: ${LONG}`, 4, { reply_to_id: "m_other" }),
  msg("m_arabic", "u_bart", "bartholomew", ARABIC, 5),
  msg("m_own", ME.id, ME.username, LONG, 6),
];
const IDS = MESSAGES.map((m) => m.id);

const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

// Wide: the main log. Narrow: a small window with the right panel open squeezes the log under 30rem.
const WIDTHS = { wide: 1440, narrow: 820 } as const;
type Width = keyof typeof WIDTHS;

async function openChannel(page: Page, skin: Skin, width: Width, lang: string) {
  await page.setViewportSize({ width: WIDTHS[width], height: 1000 });
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
        { id: GROUP_ID, name: "acme", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
      ],
      channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
      groupMembers: {
        [GROUP_ID]: [
          { user_id: ME.id, username: ME.username, role: "admin", joined_at: "2026-01-01T00:00:00Z" },
          { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
          { user_id: "u_bart", username: "bartholomew", role: "member", joined_at: "2026-01-01T00:00:00Z" },
        ],
      },
      messages: { [CHANNEL_ID]: MESSAGES },
      dmChannels: [],
      preferences: { skin, right_panel_open_by_default: width === "narrow" },
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.locator("html")).toHaveAttribute("dir", lang === "ar" ? "rtl" : "ltr");
  if (skin === "terminal") {
    await page.getByTestId("menu-item-groups").click();
    await page.getByTestId(`group-option-${GROUP_ID}`).click();
    await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  } else {
    await page.getByTestId("sidebar").getByRole("button", { name: /general$/ }).click();
  }
  await expect(page.getByTestId("message-m_own").getByTestId("message-content")).toBeVisible();

  const list = await page.getByTestId("message-list").boundingBox();
  // 30rem: the width #2116's DM-beside-a-call panel falls under.
  if (width === "narrow") {
    expect(list!.width, "the narrow case really is narrow").toBeLessThan(480);
  } else {
    expect(list!.width).toBeGreaterThan(700);
  }
}

type Line = Rect & { chars: number[]; words: { x: number; text: string }[] };

/**
 * The first painted line of the row's text: the body's first line box, joined in Terminal by the
 * name and ":" that share it. `chars` holds the x centre of each visible character on it, left to
 * right, so the pointer can aim at real glyphs.
 */
const firstLineOf = (row: Locator): Promise<Line> =>
  row.evaluate((el) => {
    const content = el.querySelector('[data-testid="message-content"]')!;
    // Terminal: name, ":" and text are one inline flow inside the content's parent.
    const author = content.parentElement!.querySelector('[data-testid="message-author"]');
    const line = author?.parentElement === content.parentElement ? content.parentElement! : content;
    // One rect per painted glyph: a block's own rect would span every line.
    const glyphs: DOMRect[] = [];
    const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = node.textContent ?? "";
      for (let i = 0; i < text.length; i++) {
        if (!text[i].trim()) {
          continue;
        }
        const range = document.createRange();
        range.setStart(node, i);
        range.setEnd(node, i + 1);
        const r = range.getBoundingClientRect();
        if (r.width > 0) {
          glyphs.push(r);
        }
      }
    }
    const top = Math.min(...glyphs.map((r) => r.top));
    const first = glyphs.filter((r) => r.top < top + 4);
    // Whole words painted on that line, to aim a double-click at.
    const words: { x: number; text: string }[] = [];
    const wordWalker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
    for (let node = wordWalker.nextNode(); node; node = wordWalker.nextNode()) {
      for (const m of (node.textContent ?? "").matchAll(/[\p{L}\p{N}]{2,}/gu)) {
        const range = document.createRange();
        range.setStart(node, m.index!);
        range.setEnd(node, m.index! + m[0].length);
        const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0);
        if (rects.length === 1 && rects[0].top < top + 4) {
          words.push({ x: rects[0].left + rects[0].width / 2, text: m[0] });
        }
      }
    }
    return {
      left: Math.min(...first.map((r) => r.left)),
      right: Math.max(...first.map((r) => r.right)),
      top,
      bottom: Math.max(...first.map((r) => r.bottom)),
      chars: first.map((r) => r.left + r.width / 2).sort((x, y) => x - y),
      words: words.sort((a, b) => a.x - b.x),
    };
  });

/** The actions are shown, clear of the first line, and inside the scroll area. */
async function expectClear(page: Page, row: Locator, label: string) {
  const actions = row.getByTestId("message-actions");
  await expect(actions).toHaveCSS("opacity", "1");
  const box = await rectOf(actions);
  const line = await firstLineOf(row);
  expect(intersects(box, line), `${label}: actions ${JSON.stringify(box)} vs line ${JSON.stringify(line)}`).toBe(false);
  const list = await rectOf(page.getByTestId("message-list"));
  expect(box.top, `${label}: actions clipped at the list's top`).toBeGreaterThanOrEqual(list.top - 0.5);
  expect(box.left).toBeGreaterThanOrEqual(list.left - 0.5);
  expect(box.right).toBeLessThanOrEqual(list.right + 0.5);
  // An opaque panel, not a see-through strip over the text.
  const bg = await actions.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(bg).not.toMatch(/rgba\(.*,\s*0\)|transparent/);
  return line;
}

for (const skin of SKINS) {
  for (const width of Object.keys(WIDTHS) as Width[]) {
    for (const lang of ["en", "ar"]) {
      const dir = lang === "ar" ? "right to left" : "left to right";
      test(`${skin}, ${width} column, ${dir}: hover actions never cover the text`, async ({ page }) => {
        await openChannel(page, skin, width, lang);
        for (const id of IDS) {
          const row = page.getByTestId(`message-${id}`);
          // Mid-list: a row flush with the log's top edge has its bar scrolled out, as anywhere.
          await row.evaluate((el) => el.scrollIntoView({ block: "center" }));
          await row.getByTestId("message-content").hover();
          const line = await expectClear(page, row, id);

          // The pointer reaches the text along the whole first line: its first, middle and last
          // glyph. A double-click on the line's first and last word selects that word while the
          // actions show.
          const y = (line.top + line.bottom) / 2;
          const n = line.chars.length;
          for (const x of [line.chars[0], line.chars[Math.floor(n / 2)], line.chars[n - 1]]) {
            const hit = await page.evaluate(([x, y]) => {
              const el = document.elementFromPoint(x, y);
              return el?.closest('[data-testid="message-actions"]') ? "actions" : "text";
            }, [x, y]);
            expect(hit, `${id}: pointer at ${x},${y}`).toBe("text");
          }
          for (const word of [line.words[0], line.words[line.words.length - 1]]) {
            await page.mouse.dblclick(word.x, y);
            const selected = await page.evaluate(() => window.getSelection()?.toString().trim() ?? "");
            expect(selected, `${id}: double-click on "${word.text}"`).toBe(word.text);
            await page.evaluate(() => window.getSelection()?.removeAllRanges());
          }
        }
        if (SHOTS) {
          await page.getByTestId("message-m_cont").hover();
          await page.getByTestId("message-list").screenshot({ path: `${SHOTS}/${skin}-${width}-${lang}.png` });
        }
      });
    }

    test(`${skin}, ${width} column: the list's first row shows its actions unclipped`, async ({ page }) => {
      await openChannel(page, skin, width, "en");
      const list = page.getByTestId("message-list");
      await list.evaluate((el) => {
        el.scrollTop = 0;
      });
      const row = page.getByTestId("message-m_first");
      await row.getByTestId("message-content").hover();
      await expectClear(page, row, "first row");
    });

    test(`${skin}, ${width} column: keyboard focus reveals the actions clear of the text`, async ({ page }) => {
      await openChannel(page, skin, width, "en");
      // Pointer off the log, so only focus-within can show them.
      await page.mouse.move(1, 1);
      await page.getByTestId("message-input").click();
      await page.mouse.move(1, 1);
      // Arrow up through the whole log from the composer, every row shown clear and unclipped,
      // including rows the walk scrolls in at the log's top edge.
      for (const id of [...IDS].reverse()) {
        await page.keyboard.press("ArrowUp");
        const row = page.getByTestId(`message-${id}`);
        await expect(row).toBeFocused();
        await expectClear(page, row, `focus-within ${id}`);
      }
      // Into the bar itself: still clear.
      await page.keyboard.press("ArrowRight");
      const first = page.getByTestId("message-m_first");
      await expect(first.getByTestId("reply-button")).toBeFocused();
      await expectClear(page, first, "focus on an action");
    });
  }
}
