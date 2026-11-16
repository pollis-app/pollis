/**
 * Every message is encrypted, so nothing says "[encrypted]", "[redacted]" or "[deleted]". A message
 * this device could not read says it could not be decrypted, and so does a quote of one that is not
 * on this device; a quote of a message sent before the viewer joined says that; a deleted message
 * and its quotes say "Message deleted".
 */

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
// No text on this device: decryption failed.
const UNREADABLE_ID = "01K1ZZZZZZZZZZZZZZZZZZZZZ1";
// A ULID from 2024: sent long before alice joined (2026-08-01), so it never reached her.
const BEFORE_JOIN_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0QQQ";
// Sent after alice joined (2026-08-14) but not on this device.
const MISSING_ID = "01KZZZZZZZZZZZZZZZZZZZZZY0";
const DELETED_ID = "01K1ZZZZZZZZZZZZZZZZZZZZZ4";
const SKINS = ["terminal", "modern"] as const;

function preloadState(skin: string) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [{ id: GROUP_ID, name: "Acme", owner_id: "u-bob", created_at: "2024-01-01T09:00:00.000Z" }],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: USER.id, username: "alice", role: "member", joined_at: "2026-08-01T09:00:00.000Z" },
        { user_id: "u-bob", username: "bob", role: "admin", joined_at: "2024-01-01T09:00:00.000Z" },
      ],
    },
    messages: {
      [CHANNEL_ID]: [
        { id: UNREADABLE_ID, conversation_id: CHANNEL_ID, sender_id: "u-bob", sender_username: "bob", sent_at: "2026-08-02T10:00:00.000Z" },
        {
          id: "01K1ZZZZZZZZZZZZZZZZZZZZZ2",
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          sender_username: "bob",
          content: "about the unreadable one",
          reply_to_id: UNREADABLE_ID,
          sent_at: "2026-08-02T10:01:00.000Z",
        },
        {
          id: "01K1ZZZZZZZZZZZZZZZZZZZZZ3",
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          sender_username: "bob",
          content: "about the old one",
          reply_to_id: BEFORE_JOIN_ID,
          sent_at: "2026-08-02T10:02:00.000Z",
        },
        {
          id: DELETED_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          sender_username: "bob",
          deleted_at: "2026-08-02T10:04:00.000Z",
          sent_at: "2026-08-02T10:03:00.000Z",
        },
        {
          id: "01K1ZZZZZZZZZZZZZZZZZZZZZ5",
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          sender_username: "bob",
          content: "about the deleted one",
          reply_to_id: DELETED_ID,
          sent_at: "2026-08-02T10:05:00.000Z",
        },
        {
          id: "01K1ZZZZZZZZZZZZZZZZZZZZZ6",
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          sender_username: "bob",
          content: "about the missing one",
          reply_to_id: MISSING_ID,
          sent_at: "2026-08-02T10:06:00.000Z",
        },
      ],
    },
  };
}

async function boot(page: Page, skin: string) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.getByTestId("sidebar").getByRole("button", { name: /general$/ }).click();
  await expect(page.getByTestId("message-list")).toBeVisible();
}

for (const skin of SKINS) {
  test(`${skin}: unreadable, missing and deleted messages and their quotes say what happened`, async ({ page }) => {
    await boot(page, skin);
    const list = page.getByTestId("message-list");

    await expect(list.getByText("This message couldn't be decrypted on this device.")).toBeVisible();
    await expect(page.getByTestId(`reply-preview-${UNREADABLE_ID}`)).toContainText("Couldn't decrypt");
    await expect(page.getByTestId(`reply-preview-${BEFORE_JOIN_ID}`)).toContainText("Sent before you joined");
    await expect(page.getByTestId(`reply-preview-${MISSING_ID}`)).toContainText("Couldn't decrypt");
    await expect(page.getByTestId(`message-${DELETED_ID}`).getByTestId("message-content")).toHaveText("Message deleted");
    await expect(page.getByTestId(`reply-preview-${DELETED_ID}`)).toContainText("Message deleted");
    await expect(list).not.toContainText(/encrypted|redacted|\[deleted\]/i);
  });
}
