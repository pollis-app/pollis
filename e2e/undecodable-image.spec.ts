// An image the webview can't decode (an iPhone HEIC on Linux or Windows) is shown as a
// downloadable file, never an "err" tile.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_heic";
const CHANNEL_ID = "c_general";
const KEY = "media/photo.heic.enc";
const MEDIA_ORIGIN = "https://media.test";

const SKINS = ["terminal", "refined"] as const;

const MESSAGES = [
  {
    id: "m_meilan",
    conversation_id: CHANNEL_ID,
    sender_id: "u_meilan",
    sender_username: "meilan",
    ciphertext: "",
    content: JSON.stringify({
      _att: [{ key: KEY, url: "", name: "IMG_0042.HEIC", ct: "image/heic", size: 2048, hash: "h-heic", kv: 1, sha: "s-heic" }],
      _txt: "",
    }),
    sent_at: "2026-11-06T20:40:00Z",
  },
];

async function boot(page: Page, skin: (typeof SKINS)[number]) {
  // HEIC's container header: bytes no browser engine here can decode as an image.
  await page.route(`${MEDIA_ORIGIN}/**`, (route) =>
    route.fulfill({ body: Buffer.from("\0\0\0\x18ftypheic\0\0\0\0mif1heic"), contentType: "image/heic" }),
  );
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [{ id: GROUP_ID, name: "heic", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" }],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: "u_meilan", username: "meilan", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: MESSAGES },
    mediaUrls: { [KEY]: `${MEDIA_ORIGIN}/photo.heic` },
    dmChannels: [],
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
}

for (const skin of SKINS) {
  test(`an undecodable image falls back to a downloadable file card — ${skin}`, async ({ page }) => {
    await boot(page, skin);
    const message = page.getByTestId("message-m_meilan");
    const card = message.getByTestId(`attachment-${KEY}`);
    await expect(card).toContainText("IMG_0042");
    await expect(card.getByRole("button", { name: /IMG_0042\.HEIC/ })).toBeVisible();
    await expect(card.locator("img")).toHaveCount(0);
    await expect(message).not.toContainText(/\berr\b/);
  });
}
