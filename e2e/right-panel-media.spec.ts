/*
 * The right panel's media grid draws the message log's own thumbnails.
 *
 * It used to have a tile of its own — no blurhash placeholder, a hairline
 * border, quarter-rounded corners, not clickable — that looked nothing like
 * the thumb the same attachment had in the chat two inches to the left. Both
 * are `AttachmentDisplay` now; these tests pin the parts that differed.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true`, so
 * `@tauri-apps/api/core` resolves to `frontend/src/__mocks__/tauri-core.ts`.
 * The mock has no media server (`get_media_url` answers null and the byte
 * fallback fails), so a thumbnail here can only ever be its blurhash — which
 * is exactly the state the old tile got wrong.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_media";
const CHANNEL_ID = "c_general";
const IMAGE_KEY = "media/photo.png";
const VIDEO_KEY = "media/clip.mp4";
const AUDIO_KEY = "media/voice.ogg";

// A valid 4x3 blurhash — `decode` throws on a malformed one and the canvas
// stays blank, which a test could not tell from "no placeholder".
const BLURHASH = "LEHV6nWB2yk8pyo0adR*.7kCMdnj";

const MESSAGES = [
  {
    id: "m1",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: JSON.stringify({
      _att: [
        { key: IMAGE_KEY, hash: "h_photo", name: "photo.png", ct: "image/png", size: 1024, bh: BLURHASH, w: 400, h: 300 },
        { key: VIDEO_KEY, hash: "h_clip", name: "clip.mp4", ct: "video/mp4", size: 4096, bh: BLURHASH, w: 640, h: 360 },
        { key: AUDIO_KEY, hash: "h_voice", name: "voice.ogg", ct: "audio/ogg", size: 2048 },
      ],
      _txt: "holiday",
    }),
    sent_at: "2026-08-01T09:00:00Z",
  },
];

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

async function openChannelWithPanel(page: Page, skin: Skin) {
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
    messages: { [CHANNEL_ID]: MESSAGES },
    dmChannels: [],
    preferences: { skin, right_panel_open_by_default: true },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-content").first()).toBeVisible();
  await expect(page.getByTestId("right-panel")).toBeVisible();
}

const boxOf = (locator: ReturnType<Page["getByTestId"]>) =>
  locator.evaluate((el) => {
    const s = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    return {
      radius: s.borderTopLeftRadius,
      borderWidth: s.borderTopWidth,
      width: Math.round(r.width),
      height: Math.round(r.height),
    };
  });

for (const skin of SKINS) {
  test.describe(`right panel media — ${skin} skin`, () => {
    test("the grid shows pictures and videos, not audio", async ({ page }) => {
      await openChannelWithPanel(page, skin);
      const tiles = page.getByTestId("right-panel-media-tile");
      await expect(tiles).toHaveCount(2);
      await expect(tiles.nth(0)).toHaveAttribute("title", "photo.png");
      await expect(tiles.nth(1)).toHaveAttribute("title", "clip.mp4");
    });

    test("a tile is the chat thumbnail: blurhash, same corners, no border, square", async ({
      page,
    }) => {
      await openChannelWithPanel(page, skin);
      const tile = page.getByTestId("right-panel-media-tile").first();
      const chatThumb = page.getByTestId(`attachment-${IMAGE_KEY}`);
      await expect(tile).toBeVisible();
      await expect(chatThumb).toBeVisible();

      // The placeholder is the blurhash, in the panel as in the log.
      await expect(tile.locator("canvas")).toHaveCount(1);
      await expect(chatThumb.locator("canvas")).toHaveCount(1);

      const tileBox = await boxOf(tile);
      const chatBox = await boxOf(chatThumb);
      expect(tileBox.radius).toBe(chatBox.radius);
      expect(tileBox.radius).not.toBe("0px");
      expect(tileBox.borderWidth).toBe("0px");
      expect(tileBox.width).toBe(tileBox.height);
    });

    test("clicking a video tile opens the lightbox", async ({ page }) => {
      await openChannelWithPanel(page, skin);
      // The image tile stays disabled until its bytes resolve, and the mock
      // has none; the video card resolves on click, which is the path that
      // opens the viewer.
      const tile = page.getByTestId("right-panel-media-tile").nth(1);
      await expect(tile).toBeEnabled();
      await tile.click();
      await expect(page.locator("video[controls]")).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(page.locator("video[controls]")).toHaveCount(0);
    });
  });
}
