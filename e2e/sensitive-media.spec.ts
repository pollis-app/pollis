// Sensitive media (#1568). SENSITIVE_SHOTS=<dir> also writes the review screenshots.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_media";
const CHANNEL_ID = "c_general";

const PHOTO_A = "media/sens-a.enc";
const PHOTO_B = "media/sens-b.enc";
const CLIP = "media/sens-clip.enc";
// Real blurhashes of the fixtures, so the cover shows their colours.
const PHOTO_BH = "LhLyBf;W{?KE,2XMSvV{[ibDJyJ#";
const CLIP_BH = "LZJ8nt=9rp@DR;NGR+ShNfR7KObE";

const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const att = (key: string, name: string, ct: string, bh: string, sens: boolean) => ({
  key,
  url: "",
  name,
  ct,
  size: 4096,
  hash: `h-${name}`,
  kv: 1,
  sha: `s-${name}`,
  bh,
  w: 640,
  h: 480,
  ...(sens ? { sens: true } : {}),
});

const MESSAGES = [
  {
    id: "m_dana",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: JSON.stringify({
      _att: [
        att(PHOTO_A, "spoiler-a.jpg", "image/jpeg", PHOTO_BH, true),
        att(PHOTO_B, "spoiler-b.jpg", "image/jpeg", PHOTO_BH, true),
        att(CLIP, "finale.webm", "video/webm", CLIP_BH, true),
      ],
      _txt: "the finale, careful",
    }),
    sent_at: "2026-10-20T09:00:00Z",
  },
];

async function boot(page: Page, skin: Skin, opts: { panel?: boolean; messages?: unknown[] } = {}) {
  await page.route(`${MEDIA_ORIGIN}/**`, (route) => {
    const file = new URL(route.request().url()).pathname.slice(1);
    route.fulfill({
      body: fs.readFileSync(path.join(FIXTURES, file)),
      contentType: file.endsWith(".webm") ? "video/webm" : "image/jpeg",
    });
  });
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
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
      ],
    },
    messages: { [CHANNEL_ID]: opts.messages ?? MESSAGES },
    mediaUrls: {
      [PHOTO_A]: `${MEDIA_ORIGIN}/sensitive-photo.jpg`,
      [PHOTO_B]: `${MEDIA_ORIGIN}/sensitive-photo.jpg`,
      [CLIP]: `${MEDIA_ORIGIN}/sensitive-clip.webm`,
    },
    dmChannels: [],
    preferences: { skin, right_panel_open_by_default: opts.panel === true },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

async function shot(page: Page, name: string, skin: Skin) {
  const dir = process.env.SENSITIVE_SHOTS;
  const file = `${name}-${skin}.png`;
  // Every picture on screen decoded, so the shot shows what a user sees.
  await page.waitForFunction(() =>
    Array.from(document.images).every((img) => img.complete && img.naturalWidth > 0),
  );
  await page.screenshot({ path: dir ? path.join(dir, file) : test.info().outputPath(file) });
}

/** Paste the photo fixture into the composer, as a screenshot paste arrives. */
async function pastePhoto(page: Page) {
  await page.getByTestId("message-input").click();
  await page.evaluate(async (url) => {
    const bytes = await (await fetch(url)).arrayBuffer();
    const file = new File([bytes], "spoiler.jpg", { type: "image/jpeg" });
    const data = new DataTransfer();
    data.items.add(file);
    const target = document.querySelector('[data-testid="message-input"]')!;
    target.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, `${MEDIA_ORIGIN}/sensitive-photo.jpg`);
}

const thumb = (page: Page, key: string) =>
  page.getByTestId("message-m_dana").getByTestId(`attachment-${key}`);

for (const skin of SKINS) {
  test.describe(`sensitive media — ${skin} skin`, () => {
    test("the composer toggle marks and unmarks, and the marked card is covered", async ({ page }) => {
      await boot(page, skin, { messages: [] });
      await pastePhoto(page);
      const card = page.getByTestId("composer-attachment");
      const toggle = card.getByTestId("attachment-sensitive-toggle");
      await expect(card.locator("img")).toBeVisible();
      await expect(toggle).toHaveAttribute("aria-pressed", "false");
      // Half again as tall as the old 80px thumb.
      const box = await card.locator("img").boundingBox();
      expect(Math.round(box!.height)).toBeGreaterThanOrEqual(118);
      await shot(page, "composer-unmarked", skin);

      await toggle.click();
      await expect(card).toHaveAttribute("data-sensitive", "true");
      await expect(toggle).toHaveAttribute("aria-pressed", "true");
      await expect(card.getByTestId("sensitive-cover")).toBeVisible();
      await expect(card.locator("img")).toHaveCount(0);
      // The cover is the picture's blurhash, worked out from the preview.
      await expect(card.getByTestId("sensitive-cover").locator("canvas")).toHaveCount(1);
      await shot(page, "composer-marked", skin);

      await toggle.click();
      await expect(card).toHaveAttribute("data-sensitive", "false");
      await expect(card.getByTestId("sensitive-cover")).toHaveCount(0);
      await expect(card.locator("img")).toBeVisible();
    });

    test("a marked attachment is sent sensitive, and the sender's copy is covered", async ({ page }) => {
      await boot(page, skin, { messages: [] });
      await pastePhoto(page);
      const card = page.getByTestId("composer-attachment");
      await card.getByTestId("attachment-sensitive-toggle").click();
      await expect(card.getByTestId("sensitive-cover").locator("canvas")).toHaveCount(1);
      await page.getByTestId("message-send-button").click();

      await expect.poll(() =>
        page.evaluate(() => {
          const args = (window as any).__tauriLastArgs.send_message;
          return args?.attachments?.[0]?.sensitive ?? null;
        }),
      ).toBe(true);
      const stored = await page.evaluate((channel) => {
        const msgs = (window as any).__tauriMock.messages[channel] as Array<{ content: string }>;
        return JSON.parse(msgs[msgs.length - 1].content);
      }, CHANNEL_ID);
      expect(stored._att[0].sens).toBe(true);
      // The composer's blurhash travels with it, so the cover has colour
      // before the upload has produced its own.
      expect(typeof stored._att[0].bh).toBe("string");

      const sent = page.locator('[data-testid^="attachment-media/mock-"]');
      await expect(sent).toHaveAttribute("data-sensitive", "hidden");
      await expect(sent.getByTestId("sensitive-cover")).toBeVisible();
    });

    test("a received sensitive picture is covered; one click reveals it alone; the eye hides it", async ({ page }) => {
      await boot(page, skin);
      for (const key of [PHOTO_A, PHOTO_B, CLIP]) {
        await expect(thumb(page, key)).toHaveAttribute("data-sensitive", "hidden");
        await expect(thumb(page, key).getByTestId("sensitive-cover")).toBeVisible();
      }
      // Nothing of the media is on screen.
      await expect(page.getByTestId("message-m_dana").locator(`img[src^="${MEDIA_ORIGIN}"]`)).toHaveCount(0);
      await shot(page, "receiver-covered", skin);

      await thumb(page, PHOTO_A).click();
      await expect(thumb(page, PHOTO_A)).toHaveAttribute("data-sensitive", "revealed");
      await expect(thumb(page, PHOTO_A).locator(`img[src^="${MEDIA_ORIGIN}"]`)).toBeVisible();
      // The reveal opened nothing: no lightbox.
      await expect(page.locator(`img[style*="90vw"]`)).toHaveCount(0);
      // Its neighbours stay covered.
      await expect(thumb(page, PHOTO_B)).toHaveAttribute("data-sensitive", "hidden");
      await expect(thumb(page, CLIP)).toHaveAttribute("data-sensitive", "hidden");
      const hide = page.getByTestId("message-m_dana").getByTestId("sensitive-hide");
      await expect(hide).toHaveCount(1);
      await shot(page, "receiver-revealed", skin);

      await hide.click();
      await expect(thumb(page, PHOTO_A)).toHaveAttribute("data-sensitive", "hidden");
      await expect(hide).toHaveCount(0);
      // As often as the user likes.
      await thumb(page, PHOTO_A).click();
      await expect(thumb(page, PHOTO_A)).toHaveAttribute("data-sensitive", "revealed");
      await page.getByTestId("message-m_dana").getByTestId("sensitive-hide").click();
      await expect(thumb(page, PHOTO_A)).toHaveAttribute("data-sensitive", "hidden");
    });

    test("the click that reveals a video neither opens nor plays it", async ({ page }) => {
      await boot(page, skin);
      await thumb(page, CLIP).click();
      await expect(thumb(page, CLIP)).toHaveAttribute("data-sensitive", "revealed");
      // Opening is async (it resolves the media URL first), so give a wrongly
      // wired reveal the time it would need before checking nothing opened.
      await page.waitForTimeout(500);
      await expect(page.locator("video")).toHaveCount(0);
      await shot(page, "video-revealed", skin);

      // A second, deliberate click opens it — paused, waiting for the user.
      await thumb(page, CLIP).click();
      const video = page.locator("video[controls]");
      await expect(video).toBeVisible();
      await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.readyState)).toBeGreaterThan(1);
      expect(await video.evaluate((v: HTMLVideoElement) => v.paused)).toBe(true);
      await shot(page, "video-opened-paused", skin);
    });

    test("the media grid and the lightbox share the reveal", async ({ page }) => {
      await boot(page, skin, { panel: true });
      await expect(page.getByTestId("right-panel")).toBeVisible();
      const tile = (name: string) =>
        page.getByTestId("right-panel").locator(`[data-testid="right-panel-media-tile"][title="${name}"]`);
      await expect(tile("spoiler-a.jpg")).toHaveAttribute("data-sensitive", "hidden");

      // Revealed in the message: revealed in the grid, and only that one.
      await thumb(page, PHOTO_A).click();
      await expect(tile("spoiler-a.jpg")).toHaveAttribute("data-sensitive", "revealed");
      await expect(tile("spoiler-b.jpg")).toHaveAttribute("data-sensitive", "hidden");
      await expect(tile("finale.webm")).toHaveAttribute("data-sensitive", "hidden");
      await shot(page, "grid-shared", skin);

      // The lightbox opens on the revealed picture, not a cover.
      await tile("spoiler-a.jpg").click();
      await expect(page.locator(`img[style*="90vw"]`)).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(page.locator(`img[style*="90vw"]`)).toHaveCount(0);

      // Hidden from the grid: hidden in the message.
      await page.getByTestId("right-panel").getByTestId("sensitive-hide").click();
      await expect(thumb(page, PHOTO_A)).toHaveAttribute("data-sensitive", "hidden");
      await expect(tile("spoiler-a.jpg")).toHaveAttribute("data-sensitive", "hidden");
    });
  });
}
