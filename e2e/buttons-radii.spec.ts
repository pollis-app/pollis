// Hand-rolled buttons and radii onto the primitives (#1967), both skins: icon buttons are named and
// drawn at the control radius, preset chips at the chip radius, no button carries an inline radius,
// and the lightbox and the voice tray use the shared controls. Screenshots of Preferences, Voice &
// video, the breadcrumb bar, a channel with the message action bar and its menu, the lightbox and
// the voice stage land in `e2e/artifacts/buttons/` for review.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_btn";
const CHANNEL_ID = "c_general";
const VOICE_CHANNEL_ID = "c_lounge";
const PHOTO = "media/btn-photo.enc";
const MEDIA_ORIGIN = "https://media.test";
const FIXTURES = path.join(__dirname, "fixtures");

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const SHOTS = "artifacts/buttons";

async function boot(page: Page, skin: Skin) {
  await page.route(`${MEDIA_ORIGIN}/**`, (route) => {
    route.fulfill({
      body: fs.readFileSync(path.join(FIXTURES, "sensitive-photo.jpg")),
      contentType: "image/jpeg",
    });
  });
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "studio", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: {
      [GROUP_ID]: [
        { id: CHANNEL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" },
        { id: VOICE_CHANNEL_ID, group_id: GROUP_ID, name: "lounge", channel_type: "voice" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {
      [CHANNEL_ID]: [
        {
          id: "m_text",
          conversation_id: CHANNEL_ID,
          sender_id: "u_dana",
          sender_username: "dana",
          ciphertext: "",
          content: "the new mix is up",
          sent_at: "2026-11-01T09:00:00Z",
        },
        {
          id: "m_photo",
          conversation_id: CHANNEL_ID,
          sender_id: "u_dana",
          sender_username: "dana",
          ciphertext: "",
          content: JSON.stringify({
            _att: [{ key: PHOTO, url: "", name: "cover.jpg", ct: "image/jpeg", size: 4096, hash: "h-cover", kv: 1, sha: "s-cover" }],
            _txt: "",
          }),
          sent_at: "2026-11-01T09:01:00Z",
        },
      ],
    },
    mediaUrls: { [PHOTO]: `${MEDIA_ORIGIN}/cover.jpg` },
    dmChannels: [],
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  // Screenshots and colour reads see the settled state.
  await page.addStyleTag({
    content: "*, *::before, *::after { transition: none !important; animation: none !important; }",
  });
}

async function openChannel(page: Page, channelId: string) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${channelId}`).click();
}

async function openPage(page: Page, name: string, pageTestId: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(name);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(pageTestId)).toBeVisible();
}

/** The root's value of a radius token, and an element's computed corner. */
async function radiusOf(page: Page, selector: string, token: string) {
  return page.evaluate(
    ({ selector, token }) => {
      const el = document.querySelector(selector);
      return {
        el: el ? getComputedStyle(el).borderTopLeftRadius : "missing",
        token: getComputedStyle(document.documentElement).getPropertyValue(token).trim(),
      };
    },
    { selector, token },
  );
}

/** Buttons on the page whose corner is an inline style rather than a token class. */
async function inlineRadiusButtons(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLButtonElement>("button"))
      .filter((b) => b.style.borderRadius !== "")
      .map((b) => b.dataset.testid ?? b.outerHTML.slice(0, 80)),
  );
}

for (const skin of SKINS) {
  test.describe(`buttons and radii — ${skin}`, () => {
    test("Preferences: preset chips are Chips at the chip radius, the choice is aria-pressed", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "appearance", "preferences-page");
      const chip = await radiusOf(page, '[data-testid="pref-accent-amber"]', "--radius-chip");
      expect(chip.el).toBe(chip.token);
      await expect(page.getByTestId("pref-accent-amber")).toHaveAttribute("aria-pressed", /true|false/);
      expect(await inlineRadiusButtons(page)).toEqual([]);
      // The page scrolls inside the main pane: bring the preset chips into the shot.
      await page.getByTestId("pref-background-warm").scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}/${skin}-preferences.png` });
    });

    test("Voice & video draws no inline-radius button", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "Voice & video", "voice-settings-page");
      expect(await inlineRadiusButtons(page)).toEqual([]);
      await page.screenshot({ path: `${SHOTS}/${skin}-voice-video.png`, fullPage: true });
    });

    test("the breadcrumb's settings button is a named IconButton at the control radius", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, CHANNEL_ID);
      const settings = page.getByTestId("breadcrumb-settings-button");
      await expect(settings).toHaveAttribute("aria-label", /.+/);
      const r = await radiusOf(page, '[data-testid="breadcrumb-settings-button"]', "--radius-control");
      expect(r.el).toBe(r.token);
      expect(await inlineRadiusButtons(page)).toEqual([]);
      await page.screenshot({ path: `${SHOTS}/${skin}-title-breadcrumb.png`, clip: { x: 0, y: 0, width: 1280, height: 96 } });
    });

    test("a channel's header icons and the message action bar", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, CHANNEL_ID);
      await expect(page.getByTestId("message-input")).toBeVisible();
      const mute = await radiusOf(page, '[data-testid="channel-mute-toggle"]', "--radius-control");
      expect(mute.el).toBe(mute.token);
      await expect(page.getByTestId("channel-mute-toggle")).toHaveAttribute("aria-label", /.+/);

      const row = page.getByTestId("message-m_text");
      await row.hover();
      await expect(row.getByTestId("message-actions")).toBeVisible();
      await page.screenshot({ path: `${SHOTS}/${skin}-channel-actions.png` });

      await row.getByTestId("message-actions-more").click();
      await expect(page.getByTestId("message-actions-menu")).toBeVisible();
      await expect(page.getByTestId("message-actions-menu").getByRole("menuitem").first()).toBeVisible();
      expect(await inlineRadiusButtons(page)).toEqual([]);
      await page.screenshot({ path: `${SHOTS}/${skin}-channel-menu.png` });
    });

    test("the lightbox's Download and Close are Buttons", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, CHANNEL_ID);
      await page.getByTestId(`attachment-${PHOTO}`).click();
      await expect(page.getByTestId("media-spotlight")).toBeVisible();
      await expect(page.getByTestId("lightbox-download")).toBeVisible();
      await expect(page.getByTestId("lightbox-close")).toBeVisible();
      const r = await radiusOf(page, '[data-testid="lightbox-download"]', "--radius-control");
      expect(r.el).toBe(r.token);
      expect(await inlineRadiusButtons(page)).toEqual([]);
      await page.screenshot({ path: `${SHOTS}/${skin}-lightbox.png` });
      await page.getByTestId("lightbox-close").click();
      await expect(page.getByTestId("media-spotlight")).toHaveCount(0);
    });

    test("the voice stage's tray buttons are named and on the control radius", async ({ page }) => {
      await boot(page, skin);
      await openChannel(page, VOICE_CHANNEL_ID);
      await expect(page.getByTestId("voice-join-cta")).toBeVisible();
      await page.evaluate((channelId) => {
        const store = (window as unknown as { __pollisStore: Record<string, Function> }).__pollisStore;
        store.voiceStartJoining(channelId, null);
        store.voiceJoined();
      }, VOICE_CHANNEL_ID);
      const mute = page.getByTestId("voice-tray-mute");
      await expect(mute).toBeVisible();
      await expect(mute).toHaveAttribute("aria-label", /.+/);
      const r = await radiusOf(page, '[data-testid="voice-tray-mute"]', "--radius-control");
      expect(r.el).toBe(r.token);
      expect(await inlineRadiusButtons(page)).toEqual([]);
      await page.screenshot({ path: `${SHOTS}/${skin}-voice-stage.png` });
    });
  });
}
