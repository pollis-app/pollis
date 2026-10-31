// Typing indicator (#1635), desktop half of the mobile parity: a peer's `typing` shows over the
// composer and goes on its stop, the user's own other devices never show, and typing in the
// composer publishes a start then exactly one stop when the draft is cleared.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2XCB";
const MSG_ID = "01HQ7Z3K9M2P5R8T1V4W6Y2M01";

const PRELOAD = {
  session: USER,
  profile: { id: USER.id, username: USER.username },
  groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
  channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
  dmChannels: [],
  messages: {
    [CHANNEL_ID]: [
      {
        id: MSG_ID,
        conversation_id: CHANNEL_ID,
        sender_id: "u-bob",
        content: "hello",
        sent_at: "2026-08-01T10:00:00.000Z",
      },
    ],
  },
  bookmarks: [],
};

async function boot(page: Page) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { windowLabel: "main", label: "main" },
      },
      plugins: {},
      transformCallback: () => 0,
      convertFileSrc: (path: string) => path,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, PRELOAD);
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${MSG_ID}`)).toBeVisible();
}

async function emit(page: Page, event: Record<string, unknown>) {
  await page.evaluate((e) => {
    (window as unknown as { __emitRealtimeEvent: (event: unknown) => number }).__emitRealtimeEvent(e);
  }, event);
}

function typing(userId: string, username: string, isTyping: boolean) {
  return {
    type: "typing",
    channel_id: CHANNEL_ID,
    conversation_id: null,
    user_id: userId,
    username,
    is_typing: isTyping,
  };
}

function lastPublish(page: Page) {
  return page.evaluate(
    () =>
      (window as unknown as { __tauriLastArgs: Record<string, Record<string, unknown>> }).__tauriLastArgs
        .publish_typing ?? null,
  );
}

function publishCount(page: Page) {
  return page.evaluate(
    () => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts.publish_typing ?? 0,
  );
}

test("a peer's typing shows over the composer and goes on its stop", async ({ page }) => {
  await boot(page);
  const line = page.getByTestId("typing-indicator");
  await expect(line).toHaveText("");

  await emit(page, typing("u-bob", "bob", true));
  await expect(line).toHaveText("bob is typing…");
  await emit(page, typing("u-ann", "ann", true));
  await expect(line).toHaveText("ann and bob are typing…");

  await emit(page, typing("u-ann", "ann", false));
  await emit(page, typing("u-bob", "bob", false));
  await expect(line).toHaveText("");

  // The user's own other device typing is not "someone typing".
  await emit(page, typing(USER.id, USER.username, true));
  await page.waitForTimeout(300);
  await expect(line).toHaveText("");
});

test("typing in the composer publishes a start, and clearing it one stop", async ({ page }) => {
  await boot(page);
  const input = page.getByTestId("message-input");
  await input.click();
  await page.keyboard.type("hi bob", { delay: 5 });

  await expect.poll(() => lastPublish(page)).toMatchObject({
    roomId: GROUP_ID,
    channelId: CHANNEL_ID,
    conversationId: null,
    isTyping: true,
  });
  // Throttled: one start for the whole burst.
  expect(await publishCount(page)).toBe(1);

  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyA" : "Control+KeyA");
  await page.keyboard.press("Backspace");
  await expect.poll(() => lastPublish(page)).toMatchObject({ isTyping: false });
  expect(await publishCount(page)).toBe(2);
});
