// A failed image is never sticky (#1826): once the same file loads anywhere (another row, the spotlight)
// every row and spotlight still showing "failed" draws it at once, and a remount asks again. No polling.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_recover";
const GENERAL = "c_general";
const RANDOM = "c_random";

const PHOTO = "media/recover-photo.enc";
const OTHER = "media/recover-other.enc";
const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";

const att = (key: string, name: string) => ({
  key,
  url: "",
  name,
  ct: "image/jpeg",
  size: 4096,
  hash: `h-${key}`,
  kv: 1,
  sha: `s-${key}`,
});

const message = (id: string, atts: ReturnType<typeof att>[], at: string) => ({
  id,
  conversation_id: GENERAL,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: JSON.stringify({ _att: atts, _txt: "" }),
  sent_at: at,
});

async function boot(page: Page, failMediaKeys: string[]) {
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
      { id: GROUP_ID, name: "recover", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: {
      [GROUP_ID]: [
        { id: GENERAL, group_id: GROUP_ID, name: "general" },
        { id: RANDOM, group_id: GROUP_ID, name: "random" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {
      [GENERAL]: [
        message("m_first", [att(PHOTO, "first.jpg"), att(OTHER, "other.jpg")], "2026-11-01T09:00:00Z"),
      ],
      [RANDOM]: [],
    },
    mediaUrls: {
      [PHOTO]: `${MEDIA_ORIGIN}/photo.jpg`,
      [OTHER]: `${MEDIA_ORIGIN}/other.jpg`,
    },
    failMediaKeys,
    dmChannels: [],
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

const thumb = (page: Page, key: string) => page.getByTestId(`attachment-${key}`);

/** The network comes back for `key`. */
async function restore(page: Page, key: string) {
  await page.evaluate((key) => {
    (window as unknown as { __tauriFailMedia: (k: string, f: boolean) => void }).__tauriFailMedia(key, false);
  }, key);
}

/** Dana posts the same picture again; its row fetches it fresh. */
async function repost(page: Page) {
  await page.evaluate(
    ({ channelId, msg }) => {
      const w = window as unknown as {
        __tauriMock: { messages: Record<string, unknown[]> };
        __emitRealtimeEvent: (event: unknown) => number;
      };
      w.__tauriMock.messages[channelId].push(msg);
      w.__emitRealtimeEvent({ type: "new_message", channel_id: channelId, conversation_id: null, sender_id: "u_dana" });
    },
    { channelId: GENERAL, msg: message("m_again", [att(PHOTO, "again.jpg")], new Date().toISOString()) },
  );
}

async function expectFailed(page: Page, key: string) {
  await expect(thumb(page, key).first()).toHaveAttribute("data-media-state", "failed");
  await expect(thumb(page, key).first().locator("img")).toHaveCount(0);
}

async function expectDrawn(page: Page, key: string) {
  for (const card of await thumb(page, key).all()) {
    await expect(card).toHaveAttribute("data-media-state", "ready");
    await expect(card.locator("img")).toBeVisible();
  }
}

test("a failed row draws the picture as soon as another row loads the same file", async ({ page }) => {
  await boot(page, [PHOTO]);
  await expectFailed(page, PHOTO);
  await expectDrawn(page, OTHER);

  await restore(page, PHOTO);
  // Nothing re-asks on its own: no timer, the row stays failed until the file loads somewhere.
  await page.waitForTimeout(500);
  await expectFailed(page, PHOTO);

  await repost(page);
  await expect(thumb(page, PHOTO)).toHaveCount(2);
  await expectDrawn(page, PHOTO);
});

test("the spotlight's 'failed to load' clears when the file loads elsewhere, and so does the row", async ({ page }) => {
  await boot(page, [PHOTO]);
  await expectFailed(page, PHOTO);
  // Open the working picture and step back to the failed one.
  await thumb(page, OTHER).click();
  const spot = page.getByTestId("media-spotlight");
  await expect(spot.locator('img[alt="other.jpg"]')).toBeVisible();
  await page.getByTestId("media-spotlight-prev").click();
  await expect(spot.getByTestId("media-spotlight-placeholder")).toContainText("Failed to load");

  await restore(page, PHOTO);
  await repost(page);
  await expect(spot.locator('img[alt="first.jpg"]')).toBeVisible();
  await expect(spot.getByTestId("media-spotlight-placeholder")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expectDrawn(page, PHOTO);
});

test("a remount after a failure asks again instead of keeping the old error", async ({ page }) => {
  await boot(page, [PHOTO]);
  await expectFailed(page, PHOTO);
  await restore(page, PHOTO);

  // Away through the sidebar, which unmounts the row, and back.
  const sidebar = page.getByTestId("sidebar");
  await sidebar.getByRole("button", { name: "random", exact: true }).click();
  await expect(thumb(page, PHOTO)).toHaveCount(0);
  await sidebar.getByRole("button", { name: "general", exact: true }).click();
  await expectDrawn(page, PHOTO);
});
