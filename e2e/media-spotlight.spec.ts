// The media spotlight steps through one message's images and videos (#1788): arrows, ← / →, a position, wrapping as mobile does.
// SPOTLIGHT_SHOTS=<dir> also writes a review screenshot per skin.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_spot";
const CHANNEL_ID = "c_general";

const PHOTO_A = "media/spot-a.enc";
const CLIP = "media/spot-clip.enc";
const PHOTO_B = "media/spot-b.enc";
const DOC = "media/spot-doc.enc";
const LONE = "media/spot-lone.enc";

const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const att = (key: string, name: string, ct: string) => ({
  key,
  url: "",
  name,
  ct,
  size: 4096,
  hash: `h-${name}`,
  kv: 1,
  sha: `s-${name}`,
});

const MESSAGES = [
  {
    id: "m_roll",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: JSON.stringify({
      _att: [
        att(PHOTO_A, "first.jpg", "image/jpeg"),
        att(DOC, "notes.pdf", "application/pdf"),
        att(CLIP, "middle.webm", "video/webm"),
        att(PHOTO_B, "last.jpg", "image/jpeg"),
      ],
      _txt: "three from the trip",
    }),
    sent_at: "2026-11-01T09:00:00Z",
  },
  {
    id: "m_lone",
    conversation_id: CHANNEL_ID,
    sender_id: "u_dana",
    sender_username: "dana",
    ciphertext: "",
    content: JSON.stringify({ _att: [att(LONE, "alone.jpg", "image/jpeg")], _txt: "" }),
    sent_at: "2026-11-01T09:01:00Z",
  },
];

async function boot(page: Page, skin: Skin) {
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
      { id: GROUP_ID, name: "spot", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
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
      [PHOTO_A]: `${MEDIA_ORIGIN}/sensitive-photo.jpg`,
      [PHOTO_B]: `${MEDIA_ORIGIN}/sensitive-photo.jpg`,
      [LONE]: `${MEDIA_ORIGIN}/sensitive-photo.jpg`,
      [CLIP]: `${MEDIA_ORIGIN}/sensitive-clip.webm`,
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

const thumb = (page: Page, key: string) => page.getByTestId(`attachment-${key}`);

/** The spotlight shows `name` at `position`. */
async function expectShowing(page: Page, name: string, position: string) {
  const spot = page.getByTestId("media-spotlight");
  await expect(spot.getByTestId("media-spotlight-position")).toHaveText(position);
  await expect(spot).toContainText(name);
  if (name.endsWith(".webm")) {
    await expect(spot.locator("video[controls]")).toBeVisible();
  } else {
    await expect(spot.locator(`img[alt="${name}"]`)).toBeVisible();
  }
}

for (const skin of SKINS) {
  test.describe(skin, () => {
    test("arrows and ← / → walk the message's images and videos, wrapping at both ends", async ({ page }) => {
      await boot(page, skin);
      await thumb(page, PHOTO_A).click();
      await expectShowing(page, "first.jpg", "1 / 3");

      // The PDF is not in the roll: next is the video.
      await page.getByTestId("media-spotlight-next").click();
      await expectShowing(page, "middle.webm", "2 / 3");
      await page.keyboard.press("ArrowRight");
      await expectShowing(page, "last.jpg", "3 / 3");
      await page.keyboard.press("ArrowRight");
      await expectShowing(page, "first.jpg", "1 / 3");
      await page.keyboard.press("ArrowLeft");
      await expectShowing(page, "last.jpg", "3 / 3");
      await page.getByTestId("media-spotlight-prev").click();
      await expectShowing(page, "middle.webm", "2 / 3");

      // One spotlight, never a second one under it.
      await expect(page.getByTestId("media-spotlight")).toHaveCount(1);
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("media-spotlight")).toHaveCount(0);
    });

    test("opening the middle item starts there", async ({ page }) => {
      await boot(page, skin);
      await thumb(page, CLIP).click();
      await expectShowing(page, "middle.webm", "2 / 3");
    });

    test("a single picture draws no arrows or position, and the keys do nothing", async ({ page }) => {
      await boot(page, skin);
      await thumb(page, LONE).click();
      const spot = page.getByTestId("media-spotlight");
      await expect(spot.locator('img[alt="alone.jpg"]')).toBeVisible();
      await expect(spot.getByTestId("media-spotlight-prev")).toHaveCount(0);
      await expect(spot.getByTestId("media-spotlight-next")).toHaveCount(0);
      await expect(spot.getByTestId("media-spotlight-position")).toHaveCount(0);
      await page.keyboard.press("ArrowRight");
      await expect(spot.locator('img[alt="alone.jpg"]')).toBeVisible();
    });

    test("the keys are bound only while the spotlight is open", async ({ page }) => {
      await boot(page, skin);
      await thumb(page, PHOTO_A).click();
      await expectShowing(page, "first.jpg", "1 / 3");
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("media-spotlight")).toHaveCount(0);
      // Closed, an arrow neither reopens nor steps a spotlight.
      await page.keyboard.press("ArrowRight");
      await expect(page.getByTestId("media-spotlight")).toHaveCount(0);
      // Reopened, it starts at the item clicked, not where the last one stopped.
      await thumb(page, PHOTO_B).click();
      await expectShowing(page, "last.jpg", "3 / 3");
    });

    test("a click on the arrows does not close the spotlight", async ({ page }) => {
      await boot(page, skin);
      await thumb(page, PHOTO_A).click();
      await page.getByTestId("media-spotlight-next").click();
      await page.getByTestId("media-spotlight-next").click();
      await expectShowing(page, "last.jpg", "3 / 3");
      // The backdrop still closes it.
      if (process.env.SPOTLIGHT_SHOTS) {
        await page.screenshot({ path: path.join(process.env.SPOTLIGHT_SHOTS, `spotlight-${skin}.png`) });
      }
      await page.getByTestId("media-spotlight").click({ position: { x: 5, y: 5 } });
      await expect(page.getByTestId("media-spotlight")).toHaveCount(0);
    });
  });
}
