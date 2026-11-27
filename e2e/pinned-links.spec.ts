// Links in the pinned list (#2127) draw as they do in the message list, in BOTH skins: link
// styling, the system-browser open, and (Modern) the sender's preview card.
// PINNED_LINK_SHOTS=<dir> also writes review screenshots.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_pins";
const CHANNEL_ID = "c_general";

const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";
const THUMB_KEY = "media/pin-thumb.enc";

const GITHUB = "https://github.com/";
const TITLE = "GitHub · Build and ship software on a single, collaborative platform";

const LINKED = {
  id: "m_dana",
  conversation_id: CHANNEL_ID,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: `have you seen this ${GITHUB} @mia`,
  sent_at: "2026-11-17T09:00:00Z",
  link_preview: {
    url: GITHUB,
    title: TITLE,
    description: "Where the world builds software.",
    image: { key: THUMB_KEY, hash: "h-pin", kv: 1, sha: "s-pin", ct: "image/jpeg", size: 4096, w: 600, h: 450 },
  },
};

type Opts = { skin?: "modern" | "terminal"; pins?: unknown[]; dmRequests?: unknown[] };

async function boot(page: Page, opts: Opts = {}) {
  await page.route(`${MEDIA_ORIGIN}/**`, (route) =>
    route.fulfill({ body: fs.readFileSync(path.join(FIXTURES, "sensitive-photo.jpg")), contentType: "image/jpeg" }),
  );
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "pins", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: [LINKED] },
    mediaUrls: { [THUMB_KEY]: `${MEDIA_ORIGIN}/thumb.jpg` },
    pins: opts.pins ?? [],
    dmChannels: [],
    dmRequests: opts.dmRequests ?? [],
    preferences: { skin: opts.skin ?? "modern" },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openChannel(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

/** Pin the linked message through its own actions menu, then open the pinned list. */
async function pinAndOpen(page: Page) {
  const message = page.getByTestId(`message-${LINKED.id}`);
  await message.hover();
  await message.getByTestId("message-actions-more").click();
  await page.getByTestId("pin-button").click();
  await page.getByTestId("channel-pins-trigger").click();
  const row = page.getByTestId("pinned-panel-row");
  await expect(row).toHaveCount(1);
  return { message, row };
}

async function shot(page: Page, name: string) {
  const dir = process.env.PINNED_LINK_SHOTS;
  await page.waitForFunction(() =>
    Array.from(document.images).every((img) => img.complete && img.naturalWidth > 0),
  );
  await page.screenshot({ path: dir ? path.join(dir, `${name}.png`) : test.info().outputPath(`${name}.png`) });
}

const opened = (page: Page) =>
  page.evaluate(() => (window as unknown as { __pollisOpened?: string[] }).__pollisOpened ?? []);

for (const skin of ["modern", "terminal"] as const) {
  test.describe(`pinned links — ${skin}`, () => {
    test("a pinned link is the message list's link: same element, styling and safe open", async ({ page }) => {
      await boot(page, { skin });
      await openChannel(page);
      const { message, row } = await pinAndOpen(page);

      const inList = message.locator("a.message-link");
      const inPin = row.locator("a.message-link");
      await expect(inPin).toHaveCount(1);
      await expect(inPin).toHaveText(GITHUB);
      await expect(inPin).toHaveAttribute("href", GITHUB);
      await expect(inPin).toHaveAttribute("title", GITHUB);
      await expect(inPin).toHaveAttribute("dir", "ltr");
      // Same computed styling as the timeline's link.
      const style = (el: Element) => {
        const cs = getComputedStyle(el);
        return { color: cs.color, decoration: cs.textDecorationLine };
      };
      expect(await inPin.evaluate(style)).toEqual(await inList.evaluate(style));
      // The resolved mention is a token in both places.
      await expect(row.getByText("@mia")).toBeVisible();

      // A click opens the system browser, never navigates the app.
      const before = page.url();
      await inPin.click();
      await expect.poll(() => opened(page)).toEqual([GITHUB]);
      expect(page.url()).toBe(before);

      if (skin === "modern") {
        // The sender's preview card, as under the message, opening the same body link.
        const card = row.getByTestId("message-link-preview");
        await expect(card).toBeVisible();
        await expect(card).toContainText("github.com");
        await expect(card).toContainText(TITLE);
        await expect(card.locator("img")).toBeVisible();
        await card.click();
        await expect.poll(() => opened(page)).toEqual([GITHUB, GITHUB]);
      } else {
        // Terminal draws no preview cards, in the list or the pins.
        await expect(message.getByTestId("message-link-preview")).toHaveCount(0);
        await expect(row.getByTestId("message-link-preview")).toHaveCount(0);
      }
      await shot(page, `pinned-link-${skin}`);
    });
  });
}

test("a pin from before previews were sealed still shows its link", async ({ page }) => {
  await boot(page, {
    pins: [
      {
        id: "pin_old",
        conversation_id: CHANNEL_ID,
        message_id: "m_old",
        pinned_by: "u_dana",
        pinned_at: "2026-11-16T09:00:00Z",
        content: "docs at https://example.com/docs",
        sender_id: "u_dana",
        sender_username: "dana",
        sent_at: "2026-11-16T08:00:00Z",
      },
    ],
  });
  await openChannel(page);
  await page.getByTestId("channel-pins-trigger").click();
  const row = page.getByTestId("pinned-panel-row");
  await expect(row.locator("a.message-link")).toHaveAttribute("href", "https://example.com/docs");
  await expect(row.getByTestId("message-link-preview")).toHaveCount(0);
});

test("an unsafe link in a pin stays plain text, exactly as in the timeline", async ({ page }) => {
  await boot(page, {
    pins: [
      {
        id: "pin_js",
        conversation_id: CHANNEL_ID,
        message_id: "m_js",
        pinned_by: "u_dana",
        pinned_at: "2026-11-16T09:00:00Z",
        content: "click javascript:alert(1) or ftp://example.com/x",
        sender_id: "u_dana",
        sender_username: "dana",
        sent_at: "2026-11-16T08:00:00Z",
      },
    ],
  });
  await openChannel(page);
  await page.getByTestId("channel-pins-trigger").click();
  const row = page.getByTestId("pinned-panel-row");
  await expect(row.getByTestId("pinned-panel-text")).toContainText("javascript:alert(1)");
  await expect(row.locator("a")).toHaveCount(0);
});
