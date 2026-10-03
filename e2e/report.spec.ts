/**
 * Report abuse (#1213), desktop. From another person's message (and from
 * their profile), a report needs a reason, can also block, and sends ids and
 * the reason only: the message text never appears in what is sent.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const BOB_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0BOB";
const MY_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0MIN";
const SECRET = "the words only alice and bob can read";

async function boot(page: Page) {
  await page.addInitScript((d) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = d;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin: "refined" }),
    groups: [{ id: GROUP_ID, name: "Acme", owner_id: ME.id, created_at: new Date().toISOString() }],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    dmChannels: [],
    messages: {
      [CHANNEL_ID]: [
        { id: BOB_MSG, conversation_id: CHANNEL_ID, sender_id: "u-bob", content: SECRET, sent_at: "2026-08-01T10:00:00.000Z" },
        { id: MY_MSG, conversation_id: CHANNEL_ID, sender_id: ME.id, content: "mine", sent_at: "2026-08-01T10:01:00.000Z" },
      ],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openChannel(page: Page) {
  await page.getByText("general").first().click();
  await expect(page.getByText(SECRET)).toBeVisible();
}

async function reports(page: Page): Promise<Record<string, unknown>[]> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock?: { reports: Record<string, unknown>[] } }).__tauriMock?.reports ?? [],
  );
}

async function openMenuOn(page: Page, messageId: string) {
  const row = page.getByTestId(`message-${messageId}`);
  await row.hover();
  await row.getByTestId("message-actions-more").click();
}

test("reporting a message needs a reason, can block, and sends no message text", async ({ page }) => {
  await boot(page);
  await openChannel(page);

  // Your own messages have no Report.
  await openMenuOn(page, MY_MSG);
  await expect(page.getByTestId("message-actions-menu")).toBeVisible();
  await expect(page.getByTestId("report-button")).toHaveCount(0);
  await page.keyboard.press("Escape");

  await openMenuOn(page, BOB_MSG);
  await page.getByTestId("report-button").click();
  await expect(page.getByTestId("report-page")).toBeVisible();

  await expect(page.getByTestId("report-submit")).toBeDisabled();
  await page.getByTestId("report-reason-harassment").click();
  await page.getByTestId("report-and-block").click();
  await expect(page.getByTestId("report-done")).toBeVisible();

  const sent = await reports(page);
  expect(sent).toEqual([
    {
      reportedId: "u-bob",
      reason: "harassment",
      conversationId: CHANNEL_ID,
      messageId: BOB_MSG,
      alsoBlock: true,
    },
  ]);
  expect(JSON.stringify(sent)).not.toContain(SECRET);
});
