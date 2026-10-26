/*
 * Terminal skin: attachments and reactions sit in the message body column
 * (#1489). They used to start at the timestamp column, far left of the text
 * they belong to; now the image strip, a file row and the reaction pills all
 * start where the text after `name :` starts — the terminal counterpart of
 * the refined skin's right column.
 *
 * Covers a multi-attachment row, consecutive messages from one sender (which
 * the refined skin would group), and a reply. The body column's x varies
 * with the author's name width, so each row is measured against itself.
 *
 * Runs against the browser build with `VITE_PLAYWRIGHT=true` (see
 * playwright.config.js). The mock has no media server, so thumbs render
 * their blurhash — enough to measure.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_media";
const CHANNEL_ID = "c_general";
const BLURHASH = "LEHV6nWB2yk8pyo0adR*.7kCMdnj";

const image = (name: string) => ({
  key: `media/${name}.png`, hash: `h_${name}`, kv: 1, sha: `s_${name}`,
  name: `${name}.png`, ct: "image/png", size: 1024, bh: BLURHASH, w: 400, h: 300,
});
const file = (name: string) => ({
  key: `media/${name}.pdf`, hash: `h_${name}`, kv: 1, sha: `s_${name}`,
  name: `${name}.pdf`, ct: "application/pdf", size: 2048,
});

const msg = (
  id: string,
  senderId: string,
  username: string,
  text: string,
  at: string,
  attachments: unknown[],
  extra: Record<string, unknown> = {},
) => ({
  id,
  conversation_id: CHANNEL_ID,
  sender_id: senderId,
  sender_username: username,
  ciphertext: "",
  content: JSON.stringify({ _att: attachments, _txt: text }),
  sent_at: at,
  ...extra,
});

const MESSAGES = [
  // Multi-attachment row: three pictures and a file.
  msg("m_multi", "u_dana", "dana", "holiday photos", "2026-08-01T09:00:00Z", [image("a"), image("b"), image("c"), file("itinerary")]),
  // Consecutive message, same sender.
  msg("m_next", "u_dana", "dana", "one more", "2026-08-01T09:00:30Z", [image("d")]),
  // A reply from someone with a longer name, so its column sits elsewhere.
  msg("m_reply", "u_bartholomew", "bartholomew", "mine", "2026-08-01T09:01:00Z", [image("e")], { reply_to_id: "m_multi" }),
];

const REACTIONS = {
  m_multi: [{ emoji: "👍", user_ids: ["u_bartholomew"], count: 1 }],
  m_next: [{ emoji: "🎉", user_ids: [ME.id], count: 1 }],
  m_reply: [{ emoji: "❤️", user_ids: ["u_dana"], count: 1 }],
};

async function openChannel(page: Page) {
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
        { user_id: "u_bartholomew", username: "bartholomew", role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [CHANNEL_ID]: MESSAGES },
    reactions: REACTIONS,
    dmChannels: [],
    preferences: { skin: "terminal" },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${CHANNEL_ID}`).click();
  await expect(page.getByTestId("message-m_reply").getByTestId("message-content")).toBeVisible();
}

const leftOf = async (locator: ReturnType<Page["locator"]>) => {
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  return Math.round(box!.x);
};

test.describe("terminal skin — attachments line up with the message text", () => {
  for (const id of ["m_multi", "m_next", "m_reply"]) {
    test(`${id}: pictures, files and reactions start at the body column`, async ({ page }) => {
      await openChannel(page);
      const row = page.getByTestId(`message-${id}`);
      const bodyX = await leftOf(row.getByTestId("message-content"));
      const timestampX = await leftOf(row.getByTestId("message-timestamp"));
      const authorX = await leftOf(row.getByTestId("message-author"));
      // The body column is right of the timestamp and the name.
      expect(bodyX).toBeGreaterThan(authorX);
      expect(authorX).toBeGreaterThan(timestampX);

      const thumbs = row.locator('[data-testid^="attachment-media/"][data-testid$=".png"]');
      const count = await thumbs.count();
      expect(count).toBeGreaterThan(0);
      expect(await leftOf(thumbs.first())).toBe(bodyX);

      const reactions = row.getByTestId("message-reactions");
      await expect(reactions).toBeVisible();
      expect(await leftOf(reactions)).toBe(bodyX);
    });
  }

  test("a file row in a multi-attachment message starts at the body column too", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_multi");
    const bodyX = await leftOf(row.getByTestId("message-content"));
    await expect(row.locator('[data-testid^="attachment-media/"][data-testid$=".png"]')).toHaveCount(3);
    const fileRow = row.getByTestId("attachment-media/itinerary.pdf");
    await expect(fileRow).toBeVisible();
    expect(await leftOf(fileRow)).toBe(bodyX);
  });

  test("the reply preview stays above the row, and the reply's picture stays in its body column", async ({ page }) => {
    await openChannel(page);
    const row = page.getByTestId("message-m_reply");
    const preview = row.getByTestId("reply-preview-m_multi");
    await expect(preview).toBeVisible();
    const previewBox = (await preview.boundingBox())!;
    const contentBox = (await row.getByTestId("message-content").boundingBox())!;
    expect(previewBox.y + previewBox.height).toBeLessThanOrEqual(contentBox.y + 1);
    // A longer name pushes this row's column further right than dana's.
    const danaBodyX = await leftOf(page.getByTestId("message-m_multi").getByTestId("message-content"));
    expect(Math.round(contentBox.x)).toBeGreaterThan(danaBodyX);
  });
});
