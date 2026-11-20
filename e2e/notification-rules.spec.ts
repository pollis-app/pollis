// Notification fire and clear rules (#2056), desktop. Reading is marking read: a message that
// arrives in the DM the user is looking at (window focused, not idle, at the bottom) never badges
// the row, the DMS header or the bottom bar, and raises no OS notification. A blurred window
// leaves it unread and alerts, and coming back clears it. The desktop tells the DS when it is in
// use, so the phone's push is held meanwhile.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const DM_ID = "dm_bob";

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

const PRELOAD = {
  session: ME,
  profile: { id: ME.id, username: ME.username },
  groups: [],
  channels: {},
  dmChannels: [
    {
      id: DM_ID,
      created_by: "u_bob",
      created_at: "2026-01-01T00:00:00Z",
      members: [member(ME.id, "mia"), member("u_bob", "bob")],
    },
  ],
  messages: {
    [DM_ID]: [
      {
        id: "m_old",
        conversation_id: DM_ID,
        sender_id: "u_bob",
        sender_username: "bob",
        content: "an old message, already read",
        sent_at: "2026-01-01T00:00:01Z",
      },
    ],
  },
  notificationPermission: true,
  preferences: { allow_desktop_notifications: true, allow_sound_effects: true },
};

const EVIDENCE = process.env.POLLIS_EVIDENCE_DIR;

async function evidence(page: Page, name: string) {
  if (EVIDENCE) {
    await page.screenshot({ path: `${EVIDENCE}/${name}.png` });
  }
}

async function boot(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

function count(page: Page, command: string): Promise<number> {
  return page.evaluate(
    (cmd) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[cmd] ?? 0,
    command,
  );
}

function activity(page: Page): Promise<boolean[]> {
  return page.evaluate(
    () => (window as unknown as { __tauriMock: { pushActivity: boolean[] } }).__tauriMock.pushActivity,
  );
}

async function openDm(page: Page) {
  await page.getByTestId(`sidebar-row-dm-${DM_ID}`).click();
  await expect(page.getByTestId("message-m_old")).toBeVisible();
}

async function bobSays(page: Page, id: string, content: string) {
  await page.evaluate(
    ({ dm, id, content }) => {
      const w = window as unknown as { __tauriMock: { messages: Record<string, unknown[]> } };
      w.__tauriMock.messages[dm].push({
        id,
        conversation_id: dm,
        sender_id: "u_bob",
        sender_username: "bob",
        content,
        sent_at: new Date().toISOString(),
      });
    },
    { dm: DM_ID, id, content },
  );
  await page.evaluate((dm) => {
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent({
      type: "new_message",
      channel_id: null,
      conversation_id: dm,
    });
  }, DM_ID);
}

async function resetCounts(page: Page) {
  await page.evaluate(() => {
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
  });
}

test("an open, focused DM clears its badge, the section header and the bottom bar", async ({ page }) => {
  await boot(page);
  await openDm(page);
  await resetCounts(page);
  // The desktop told the DS it is in use, so the phone's push is held.
  await expect.poll(async () => (await activity(page)).at(-1)).toBe(true);

  await bobSays(page, "m_new", "are you around?");
  await expect(page.getByTestId("message-m_new")).toBeVisible();
  await expect.poll(() => count(page, "mark_conversation_read")).toBeGreaterThan(0);
  await expect(page.getByTestId(`sidebar-unread-dm-${DM_ID}`)).toHaveCount(0);
  await expect(page.getByTestId("sidebar-unread-dms")).toHaveCount(0);
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("");
  // Let the async wake run to the end, if it would alert.
  await page.waitForTimeout(500);
  await expect(page.getByTestId(`sidebar-unread-dm-${DM_ID}`)).toHaveCount(0);
  expect(await count(page, "plugin:notification|notify")).toBe(0);
  await expect(page.getByTestId("status-bar-alert")).toHaveCount(0);
  await evidence(page, "desktop-open-dm-no-badge");
});

test("a blurred window leaves the open DM unread and alerts; coming back reads it", async ({ page }) => {
  await boot(page);
  await openDm(page);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  // Away from the window: the DS is told, so the phone is pushed again.
  await expect.poll(async () => (await activity(page)).at(-1)).toBe(false);
  await resetCounts(page);

  await bobSays(page, "m_new", "are you around?");
  await expect(page.getByTestId(`sidebar-unread-dm-${DM_ID}`)).toHaveText("1");
  await expect(page.getByTestId("sidebar-unread-dms")).toHaveText("1");
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("1");
  await expect.poll(() => count(page, "plugin:notification|notify")).toBe(1);
  expect(await count(page, "mark_conversation_read")).toBe(0);
  await evidence(page, "desktop-blurred-dm-badged");

  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.getByTestId(`sidebar-unread-dm-${DM_ID}`)).toHaveCount(0);
  await expect(page.getByTestId("sidebar-unread-dms")).toHaveCount(0);
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("");
  await expect.poll(async () => (await activity(page)).at(-1)).toBe(true);
  await evidence(page, "desktop-refocused-dm-cleared");
});
