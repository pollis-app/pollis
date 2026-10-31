// Rapid sends (#1636): sending while a send is still out is never refused — every message shows
// at once, reaches the core at once in the order sent, is delivered once, and fails on its own.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", username: "bob" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MSG_BOB = "01HQ7Z3K9M2P5R8T1V4W6Y0BOB";

const BURST = ["rs1636 one", "rs1636 two", "rs1636 three"];

function preloadState(failSends: boolean) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin: "refined" }),
    failSends,
    groups: [
      { id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    messages: {
      [CHANNEL_ID]: [
        {
          id: MSG_BOB,
          conversation_id: CHANNEL_ID,
          sender_id: BOB.id,
          sender_username: BOB.username,
          content: "are you around?",
          sent_at: "2026-10-14T15:02:00.000Z",
        },
      ],
    },
  };
}

async function boot(page: Page, preload: ReturnType<typeof preloadState>) {
  await page.addInitScript((state) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = state;
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
  }, preload);
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

/** The router uses memory history; reach the channel through a permalink. */
async function openChannel(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${CHANNEL_ID}/${MSG_BOB}`);
  await expect(page.getByTestId(`message-${MSG_BOB}`)).toBeVisible();
}

async function send(page: Page, text: string) {
  await page.getByTestId("message-input").click();
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

const stored = (page: Page) =>
  page.evaluate(
    (channel) =>
      (window as any).__tauriMock.messages[channel]
        .filter((m: { content?: string }) => m.content?.startsWith("rs1636"))
        .map((m: { content: string; send_state?: string }) => ({
          content: m.content,
          send_state: m.send_state ?? null,
        })),
    CHANNEL_ID,
  );

const sendCalls = (page: Page) =>
  page.evaluate(() => (window as any).__tauriInvokeCounts.send_message ?? 0);

/** On-screen order of the burst's rows, top to bottom. */
const shownOrder = (page: Page) =>
  page
    .locator("[data-testid^='message-']", { hasText: /rs1636/ })
    .evaluateAll((rows) =>
      rows
        // Only the burst's own words: a row's textContent runs straight into its timestamp.
        .map((r) => (r.textContent ?? "").match(/rs1636 (one|two|three)(?![a-z])/)?.[0])
        .filter((t, i, all): t is string => !!t && all.indexOf(t) === i),
    );

test("a burst sent while the first send is still out is queued, in order, and delivered once each", async ({ page }) => {
  await boot(page, preloadState(false));
  await openChannel(page);
  // The first send stays in flight, as the first send in a fresh MLS group does.
  await page.evaluate(() => (window as any).__tauriHold("send_message"));

  for (const text of BURST) {
    await send(page, text);
  }

  // Every one shows at once, in the order sent, and the composer took every one.
  for (const text of BURST) {
    await expect(page.getByText(text)).toHaveCount(1);
  }
  await expect.poll(() => shownOrder(page)).toEqual(BURST);
  await expect(page.getByTestId("message-input")).not.toContainText("rs1636");
  // Each was handed to the core at once, not held behind the one in flight: the core stores
  // every send immediately and orders delivery itself (pollis-core outbox).
  await expect.poll(() => sendCalls(page)).toBe(BURST.length);
  await expect.poll(() => stored(page)).toEqual(
    BURST.map((content) => ({ content, send_state: null })),
  );
  await page.screenshot({ path: test.info().outputPath("rapid-send-queued.png") });

  await page.evaluate(() => (window as any).__tauriRelease("send_message"));

  // Delivered in the order sent, each exactly once, none failed.
  await expect.poll(() => stored(page)).toEqual(
    BURST.map((content) => ({ content, send_state: null })),
  );
  expect(await sendCalls(page)).toBe(BURST.length);
  for (const text of BURST) {
    await expect(page.getByText(text)).toHaveCount(1);
  }
  await expect.poll(() => shownOrder(page)).toEqual(BURST);
  await expect(page.locator("[data-testid^='message-retry-']")).toHaveCount(0);
});

test("a failed send in a burst is marked on its own and never holds up the next", async ({ page }) => {
  await boot(page, preloadState(true));
  await openChannel(page);

  await send(page, BURST[0]);
  await send(page, BURST[1]);
  // Each failure is its own message's, with its own Retry.
  await expect(page.locator("[data-testid^='message-retry-']")).toHaveCount(2);

  await page.evaluate(() => (window as any).__tauriFailSends(false));
  await send(page, BURST[2]);
  await expect.poll(() => stored(page)).toEqual([
    { content: BURST[0], send_state: "failed" },
    { content: BURST[1], send_state: "failed" },
    { content: BURST[2], send_state: null },
  ]);
  await expect(page.locator("[data-testid^='message-retry-']")).toHaveCount(2);
});
