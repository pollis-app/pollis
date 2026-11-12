// Video posters (#1837): a video tile always shows a still frame or a filled placeholder at its
// aspect ratio, never blank space. VIDEO_POSTER_SHOTS=<dir> also writes review screenshots.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_video";
const CHANNEL_ID = "c_general";

const NEW_CLIP = "media/new-clip.enc";
const OLD_CLIP = "media/old-clip.enc";
const BIG_OLD_CLIP = "media/big-old-clip.enc";
const CLIP_BH = "LZJ8nt=9rp@DR;NGR+ShNfR7KObE";

const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";
// A real JPEG, as the core sends it in `pv`.
const POSTER_B64 = fs.readFileSync(path.join(FIXTURES, "video-poster.jpg")).toString("base64");

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const att = (key: string, extra: Record<string, unknown>) => ({
  key,
  url: "",
  name: `${key.split("/")[1]}.webm`,
  ct: "video/webm",
  size: 30862,
  hash: `h-${key}`,
  kv: 1,
  sha: `s-${key}`,
  w: 1280,
  h: 720,
  ...extra,
});

const message = (id: string, sentAt: string, entry: unknown) => ({
  id,
  conversation_id: CHANNEL_ID,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: JSON.stringify({ _att: [entry] }),
  sent_at: sentAt,
});

const MESSAGES = [
  // A current sender: the poster rides in the envelope.
  message("m_new", "2026-11-12T09:00:00Z", att(NEW_CLIP, { bh: CLIP_BH, pv: POSTER_B64 })),
  // An older build: no `pv`, so the frame is decoded here.
  message("m_old", "2026-11-12T09:01:00Z", att(OLD_CLIP, { bh: CLIP_BH })),
  // An older build, a large file and no blurhash: the filled placeholder, nothing fetched.
  message("m_big", "2026-11-12T09:02:00Z", att(BIG_OLD_CLIP, { size: 80 * 1024 * 1024 })),
];

async function boot(page: Page, skin: Skin, fetched: string[]) {
  await page.route(`${MEDIA_ORIGIN}/**`, (route) => {
    const file = new URL(route.request().url()).pathname.slice(1);
    fetched.push(file);
    route.fulfill({
      body: fs.readFileSync(path.join(FIXTURES, "sensitive-clip.webm")),
      contentType: "video/webm",
      // The loopback media server sends this; the frame capture needs it.
      headers: { "Access-Control-Allow-Origin": "*" },
    });
  });
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "video", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: MESSAGES },
    mediaUrls: {
      [NEW_CLIP]: `${MEDIA_ORIGIN}/new-clip.webm`,
      [OLD_CLIP]: `${MEDIA_ORIGIN}/old-clip.webm`,
      [BIG_OLD_CLIP]: `${MEDIA_ORIGIN}/big-old-clip.webm`,
    },
    dmChannels: [],
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

const tile = (page: Page, messageId: string, key: string) =>
  page.getByTestId(`message-${messageId}`).getByTestId(`attachment-${key}`);

async function shot(page: Page, name: string, skin: Skin) {
  const dir = process.env.VIDEO_POSTER_SHOTS;
  const file = `${name}-${skin}.png`;
  await page.screenshot({ path: dir ? path.join(dir, file) : test.info().outputPath(file) });
}

for (const skin of SKINS) {
  test.describe(`video posters — ${skin} skin`, () => {
    test("every video tile draws a frame or a filled placeholder at the video's aspect", async ({ page }) => {
      const fetched: string[] = [];
      await boot(page, skin, fetched);

      // The sender's poster draws at once, from the envelope alone.
      const fresh = tile(page, "m_new", NEW_CLIP);
      await expect(fresh).toHaveAttribute("data-poster", "pv");
      const img = fresh.locator('img[src^="data:image/jpeg;base64,"]');
      await expect(img).toBeVisible();
      await expect.poll(() => img.evaluate((i: HTMLImageElement) => i.naturalWidth)).toBeGreaterThan(0);

      // An older sender's video: a frame decoded here from the (small) file.
      const old = tile(page, "m_old", OLD_CLIP);
      await expect(old).toHaveAttribute("data-poster", "local");
      await expect(old.locator('img[src^="blob:"]')).toBeVisible();

      // Too big to fetch for a frame, and no blurhash: a filled tile with the play glyph.
      const big = tile(page, "m_big", BIG_OLD_CLIP);
      await expect(big).toHaveAttribute("data-poster", "placeholder");
      await expect(big.locator("svg")).toBeVisible();
      const fill = await big.evaluate((el) => getComputedStyle(el).backgroundColor);
      expect(fill).not.toBe("rgba(0, 0, 0, 0)");
      expect(fill).not.toBe("transparent");

      // 96px tall at 16:9.
      for (const t of [fresh, old, big]) {
        const box = (await t.boundingBox())!;
        expect(Math.round(box.height)).toBe(96);
        expect(Math.round(box.width)).toBe(171);
      }

      // Only the posterless small video was fetched; nothing for the poster that came along.
      expect([...new Set(fetched)]).toEqual(["old-clip.webm"]);
      await shot(page, "video-posters", skin);
    });

    test("a sent video hands its frame to the core and shows the poster that comes back", async ({ page }) => {
      const fetched: string[] = [];
      await boot(page, skin, fetched);
      await page.getByTestId("message-input").click();
      await page.evaluate(async (b64) => {
        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        const file = new File([bytes], "holiday.webm", { type: "video/webm" });
        const data = new DataTransfer();
        data.items.add(file);
        const target = document.querySelector('[data-testid="message-input"]')!;
        target.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
      }, fs.readFileSync(path.join(FIXTURES, "sensitive-clip.webm")).toString("base64"));

      // The composer card shows the captured frame.
      const card = page.getByTestId("composer-attachment");
      await expect(card.locator('img[src^="blob:"]')).toBeVisible();
      await page.getByTestId("message-send-button").click();

      await expect.poll(() =>
        page.evaluate(() => {
          const args = (window as any).__tauriLastArgs.send_message;
          return (args?.attachments?.[0]?.poster as string | undefined)?.slice(0, 4) ?? null;
        }),
      ).toBe("/9j/");

      // The stored row carries it as `pv`, and the tile draws it.
      const sent = page.locator('[data-testid^="attachment-media/mock-"]');
      await expect(sent).toHaveAttribute("data-poster", "pv");
      await expect(sent.locator('img[src^="data:image/jpeg;base64,"]')).toBeVisible();
    });
  });
}
