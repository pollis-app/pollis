// Cross-device unread (#1628), desktop: a wake alerts only for real news, badges come from Rust's
// counts, and a read made on another device clears the badge and the status-bar alert when the
// DS nudge (or a window focus) brings it in.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const DM_ID = "dm_bob";
const OLD = "m_old";

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
        id: OLD,
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

async function boot(page: Page) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.evaluate(() => {
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
  });
}

function count(page: Page, command: string): Promise<number> {
  return page.evaluate(
    (cmd) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[cmd] ?? 0,
    command,
  );
}

async function emit(page: Page, event: Record<string, unknown>) {
  await page.evaluate((e) => {
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent(e);
  }, event);
}

const DM_WAKE = { type: "new_message", channel_id: null, conversation_id: DM_ID };

test("a receipt wake (nothing new stored) neither alerts nor badges", async ({ page }) => {
  await boot(page);
  // What another device's launch does to this one: its delivered receipts wake the DM.
  await emit(page, DM_WAKE);
  await expect.poll(() => count(page, "ingest_dm_envelopes")).toBe(1);
  await expect.poll(() => count(page, "get_unread_counts")).toBeGreaterThan(0);
  // Let the async wake run to the end, if it would alert.
  await page.waitForTimeout(500);
  expect(await count(page, "plugin:notification|notify")).toBe(0);
  expect(await count(page, "play_sfx")).toBe(0);
  await expect(page.getByTestId("status-bar-alert")).toHaveCount(0);
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("");
});

test("a new message alerts and badges; a read on another device clears both on the nudge", async ({ page }) => {
  await boot(page);
  await page.evaluate((dm) => {
    const w = window as unknown as { __tauriMock: { messages: Record<string, unknown[]> } };
    w.__tauriMock.messages[dm].push({
      id: "m_new",
      conversation_id: dm,
      sender_id: "u_bob",
      sender_username: "bob",
      content: "are you around?",
      sent_at: new Date().toISOString(),
    });
  }, DM_ID);
  await emit(page, DM_WAKE);
  await expect(page.getByTestId("status-bar-alert")).toContainText("bob");
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("1");
  await expect.poll(() => count(page, "plugin:notification|notify")).toBe(1);

  // The same wake again (a receipt for it, say) is not news a second time.
  await emit(page, DM_WAKE);
  await expect.poll(() => count(page, "ingest_dm_envelopes")).toBe(2);
  await page.waitForTimeout(500);
  expect(await count(page, "plugin:notification|notify")).toBe(1);
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("1");

  // The phone reads it and pushes; the DS nudges this device's inbox.
  await page.evaluate((dm) => {
    (window as unknown as { __tauriMock: { readElsewhere: Record<string, string> } }).__tauriMock.readElsewhere = {
      [dm]: "m_new",
    };
  }, DM_ID);
  await emit(page, { type: "read_cursors_changed" });
  await expect(page.getByTestId("status-bar-alert")).toHaveCount(0);
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("");
});

// The prod repro: the message was already here and unread; the user reads it on another device,
// whose read receipt wakes this one minutes later. No sound, no banner, no alert, no extra unread;
// the nudge that follows the other device's cursor push clears the badge.
test("the other device's read receipt is silent here, and its nudge clears the badge", async ({ page }) => {
  // No read cursor yet: the old message is unread here.
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = { ...data, readCursors: {} };
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("1");
  await page.evaluate(() => {
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
  });

  // Device B reads it: its cursor is pushed, and its read receipt wakes this device.
  await page.evaluate((dm) => {
    (window as unknown as { __tauriMock: { readElsewhere: Record<string, string> } }).__tauriMock.readElsewhere = {
      [dm]: "m_old",
    };
  }, DM_ID);
  await emit(page, DM_WAKE);
  await expect.poll(() => count(page, "ingest_dm_envelopes")).toBe(1);
  await page.waitForTimeout(500);
  expect(await count(page, "plugin:notification|notify")).toBe(0);
  expect(await count(page, "play_sfx")).toBe(0);
  await expect(page.getByTestId("status-bar-alert")).toHaveCount(0);
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("1");

  await emit(page, { type: "read_cursors_changed" });
  await expect(page.getByTestId("status-bar-dms-unread")).toHaveText("");
  expect(await count(page, "plugin:notification|notify")).toBe(0);
});

test("a read made elsewhere is learned on window focus and on a realtime reconnect", async ({ page }) => {
  await boot(page);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => count(page, "refresh_read_cursors")).toBe(1);
  await emit(page, { type: "realtime_reconnected", room_id: `inbox-${ME.id}` });
  await expect.poll(() => count(page, "refresh_read_cursors")).toBe(2);
});
