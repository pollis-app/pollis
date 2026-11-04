// Vault media tiles (desktop parity for the mobile fix): the Media grid's tile is shaped like the
// chat card, and a picture already drawn in one view is drawn on the first render of the other.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const PHOTO = "media/vault-photo.enc";
const PHOTO_BH = "LhLyBf;W{?KE,2XMSvV{[ibDJyJ#";
const MEDIA_ORIGIN = "https://media.test";
const FIXTURES = path.join(__dirname, "fixtures");

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

const ENTRY = {
  id: "v_photo",
  user_id: ME.id,
  content: JSON.stringify({
    _att: [
      {
        key: PHOTO,
        url: "",
        name: "trip.jpg",
        ct: "image/jpeg",
        size: 4096,
        hash: "h-trip",
        kv: 0,
        sha: "s-trip",
        bh: PHOTO_BH,
        w: 640,
        h: 480,
      },
    ],
    _txt: "trip",
  }),
  created_at: "2026-10-20T09:00:00Z",
  updated_at: "2026-10-20T09:00:00Z",
  pinned: false,
};

async function openVault(page: Page, skin: Skin) {
  let mediaFetches = 0;
  await page.route(`${MEDIA_ORIGIN}/**`, (route) => {
    mediaFetches += 1;
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
    groups: [],
    channels: {},
    messages: {},
    dmChannels: [],
    vaultMessages: [ENTRY],
    mediaUrls: { [PHOTO]: `${MEDIA_ORIGIN}/vault-photo.jpg` },
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar-row-vault").click();
  return () => mediaFetches;
}

const radiusOf = (page: Page, selector: string) =>
  page.locator(selector).first().evaluate((el) => parseFloat(getComputedStyle(el).borderTopLeftRadius));

for (const skin of SKINS) {
  test.describe(`vault media tiles — ${skin} skin`, () => {
    test("the grid tile has the chat card's corners and no frame", async ({ page }) => {
      await openVault(page, skin);
      const card = `[data-testid="attachment-${PHOTO}"]`;
      await expect(page.locator(`${card} img`)).toBeVisible();
      const chatRadius = await radiusOf(page, card);

      await page.getByTestId("vault-view-media").click();
      const tile = '[data-testid="media-gallery-tile"]';
      await expect(page.locator(`${tile} img`)).toBeVisible();
      expect(chatRadius).toBeGreaterThan(0);
      expect(await radiusOf(page, tile)).toBe(chatRadius);
      const border = await page.locator(tile).first().evaluate((el) => parseFloat(getComputedStyle(el).borderTopWidth));
      expect(border).toBe(0);
      await page.screenshot({ path: test.info().outputPath(`vault-media-grid-${skin}.png`) });
    });

    test("switching Chat and Media draws the picture on the first render, every time", async ({ page }) => {
      await openVault(page, skin);
      await expect(page.locator(`[data-testid="attachment-${PHOTO}"] img`)).toBeVisible();
      // A MutationObserver reads the DOM of the very commit that mounts the view, before any
      // effect's fetch could fill a URL in and re-render.
      const switchTo = (view: string, selector: string) =>
        page.evaluate(
          ({ view, selector }) =>
            new Promise<{ img: boolean; canvas: boolean }>((resolve) => {
              const observer = new MutationObserver(() => {
                const box = document.querySelector(selector);
                if (box) {
                  observer.disconnect();
                  resolve({ img: !!box.querySelector("img[src]"), canvas: !!box.querySelector("canvas") });
                }
              });
              observer.observe(document.body, { childList: true, subtree: true, attributes: true });
              (document.querySelector(`[data-testid="vault-view-${view}"]`) as HTMLElement).click();
            }),
          { view, selector },
        );
      for (let i = 0; i < 3; i++) {
        expect(await switchTo("media", '[data-testid="media-gallery-tile"]')).toEqual({ img: true, canvas: false });
        await expect(page.locator('[data-testid="media-gallery-tile"] img')).toBeVisible();
        expect(await switchTo("chat", `[data-testid="attachment-${PHOTO}"]`)).toEqual({ img: true, canvas: false });
        await expect(page.locator(`[data-testid="attachment-${PHOTO}"] img`)).toBeVisible();
      }
    });
  });
}
