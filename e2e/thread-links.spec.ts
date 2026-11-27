// Links in the thread panel (#2143) draw as they do in the message list, in BOTH skins: the
// link element and its safe open, formatting (#2142), mentions, (Modern) the sender's preview
// card and the click-to-load media tiles. THREAD_LINK_SHOTS=<dir> also writes review screenshots.

import fs from "node:fs";
import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_threads";
const CHANNEL_ID = "c_general";

const FIXTURES = path.join(__dirname, "fixtures");
const MEDIA_ORIGIN = "https://media.test";
const THUMB_KEY = "media/thread-thumb.enc";

const GITHUB = "https://github.com/";
const TITLE = "GitHub · Build and ship software on a single, collaborative platform";
const IMAGE = `${MEDIA_ORIGIN}/photo.jpg`;

const ROOT = {
  id: "m_root",
  conversation_id: CHANNEL_ID,
  sender_id: ME.id,
  sender_username: ME.username,
  ciphertext: "",
  content: "where do we keep the code?",
  sent_at: "2026-11-17T09:00:00Z",
};

const LINKED_REPLY = {
  id: "m_reply_link",
  conversation_id: CHANNEL_ID,
  thread_id: ROOT.id,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: `**here** ${GITHUB} @mia`,
  sent_at: "2026-11-17T09:01:00Z",
  link_preview: {
    url: GITHUB,
    title: TITLE,
    description: "Where the world builds software.",
    image: { key: THUMB_KEY, hash: "h-thr", kv: 1, sha: "s-thr", ct: "image/jpeg", size: 4096, w: 600, h: 450 },
  },
};

const IMAGE_REPLY = {
  id: "m_reply_img",
  conversation_id: CHANNEL_ID,
  thread_id: ROOT.id,
  sender_id: "u_dana",
  sender_username: "dana",
  ciphertext: "",
  content: `and a picture ${IMAGE}`,
  sent_at: "2026-11-17T09:02:00Z",
  link_preview: null,
};

async function boot(page: Page, skin: "modern" | "terminal") {
  await page.route(`${MEDIA_ORIGIN}/**`, (route) =>
    route.fulfill({ body: fs.readFileSync(path.join(FIXTURES, "sensitive-photo.jpg")), contentType: "image/jpeg" }),
  );
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "threads", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_dana", username: "dana", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: [ROOT] },
    threadMessages: { [ROOT.id]: [LINKED_REPLY, IMAGE_REPLY] },
    threadSummaries: { [CHANNEL_ID]: [{ thread_id: ROOT.id, reply_count: 2 }] },
    mediaUrls: { [THUMB_KEY]: `${MEDIA_ORIGIN}/thumb.jpg` },
    dmChannels: [],
    dmRequests: [],
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

/** Open the thread through the root's action menu (the reply-count button is Modern-only). */
async function openThread(page: Page) {
  const root = page.getByTestId(`message-${ROOT.id}`);
  await root.hover();
  await root.getByTestId("message-actions-more").click();
  await page.getByTestId("message-actions-menu").getByTestId("thread-button").click();
  await expect(page.getByTestId("thread-root")).toBeVisible();
  const replies = page.getByTestId("thread-reply");
  await expect(replies).toHaveCount(2);
  return { linked: replies.nth(0), image: replies.nth(1) };
}

async function shot(page: Page, name: string) {
  const dir = process.env.THREAD_LINK_SHOTS;
  await page.waitForFunction(() =>
    Array.from(document.images).every((img) => img.complete && img.naturalWidth > 0),
  );
  await page.screenshot({ path: dir ? path.join(dir, `${name}.png`) : test.info().outputPath(`${name}.png`) });
}

const opened = (page: Page) =>
  page.evaluate(() => (window as unknown as { __pollisOpened?: string[] }).__pollisOpened ?? []);

for (const skin of ["modern", "terminal"] as const) {
  test.describe(`thread links — ${skin}`, () => {
    test("a link in a thread reply is a link, with its preview, as in the conversation", async ({ page }) => {
      await boot(page, skin);
      const { linked, image } = await openThread(page);

      const link = linked.locator("a.message-link");
      await expect(link).toHaveCount(1);
      await expect(link).toHaveText(GITHUB);
      await expect(link).toHaveAttribute("href", GITHUB);
      await expect(link).toHaveAttribute("dir", "ltr");
      // Formatting (#2142) and the resolved mention go through the same body.
      await expect(linked.locator("strong")).toHaveText("here");
      await expect(linked.getByText("@mia")).toBeVisible();

      // A click opens the system browser, never navigates the app.
      const before = page.url();
      await link.click();
      await expect.poll(() => opened(page)).toEqual([GITHUB]);
      expect(page.url()).toBe(before);

      if (skin === "modern") {
        // The sender's preview card, opening the same body link.
        const card = linked.getByTestId("message-link-preview");
        await expect(card).toBeVisible();
        await expect(card).toContainText("github.com");
        await expect(card).toContainText(TITLE);
        await expect(card.locator("img")).toBeVisible();
        await card.click();
        await expect.poll(() => opened(page)).toEqual([GITHUB, GITHUB]);
      } else {
        // Terminal draws no preview cards, in the timeline or the thread.
        await expect(linked.getByTestId("message-link-preview")).toHaveCount(0);
      }

      // A media link keeps its click-to-load tile in both looks, as in the timeline.
      await expect(image.locator("a.message-link")).toHaveAttribute("href", IMAGE);
      await expect(image.getByTestId("media-link-unfurl")).toBeVisible();
      await expect(image.getByTestId("message-link-preview")).toHaveCount(0);
      await shot(page, `thread-link-${skin}`);
    });
  });
}
