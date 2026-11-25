// #2099: one draft is sent once. A second submit before the input renders cleared (Enter and a
// click in one task, a double click) sends nothing more, the input is empty after the send, and
// an input method's committing Enter never sends.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", username: "bob" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MSG_BOB = "01HQ7Z3K9M2P5R8T1V4W6Y0BOB";

const preload = {
  session: USER,
  profile: { id: USER.id, username: USER.username },
  preferences: JSON.stringify({ skin: "modern" }),
  groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
  channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
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

async function boot(page: Page) {
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
  // The router uses memory history; reach the channel through a permalink.
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await page.getByTestId("search-panel-input").fill(`pollis://m/${CHANNEL_ID}/${MSG_BOB}`);
  await expect(page.getByTestId(`message-${MSG_BOB}`)).toBeVisible();
}

const sendCalls = (page: Page) =>
  page.evaluate(() => (window as any).__tauriInvokeCounts.send_message ?? 0);

const storedCopies = (page: Page, text: string) =>
  page.evaluate(
    ([channel, content]) =>
      (window as any).__tauriMock.messages[channel].filter((m: { content?: string }) => m.content === content)
        .length,
    [CHANNEL_ID, text] as const,
  );

async function typeDraft(page: Page, text: string) {
  await page.getByTestId("message-input").click();
  await page.keyboard.type(text);
  await expect(page.getByTestId("message-input")).toContainText(text);
}

/** Enter keydowns dispatched to the input in ONE task, before React can render in between. */
function enterInOneTask(page: Page, inits: KeyboardEventInit[], thenClickSend = false) {
  return page.evaluate(
    ([events, click]) => {
      const input = document.querySelector("[data-testid='message-input']") as HTMLElement;
      for (const init of events) {
        input.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Enter", code: "Enter", bubbles: true, cancelable: true, ...init }),
        );
      }
      if (click) {
        (document.querySelector("[data-testid='message-send-button']") as HTMLElement).click();
      }
    },
    [inits, thenClickSend] as const,
  );
}

test("Enter and the send button in one task send the draft once and leave the input empty", async ({ page }) => {
  await boot(page);
  const text = "dbl2099 enter plus click";
  await typeDraft(page, text);

  await enterInOneTask(page, [{}, {}], true);

  await expect(page.getByText(text)).toHaveCount(1);
  await expect.poll(() => storedCopies(page, text)).toBe(1);
  expect(await sendCalls(page)).toBe(1);
  await expect(page.getByTestId("message-input")).not.toContainText("dbl2099");
});

test("a double click on Send sends once", async ({ page }) => {
  await boot(page);
  const text = "dbl2099 double click";
  await typeDraft(page, text);

  await page.getByTestId("message-send-button").dblclick();

  await expect(page.getByText(text)).toHaveCount(1);
  await expect.poll(() => storedCopies(page, text)).toBe(1);
  expect(await sendCalls(page)).toBe(1);
  await expect(page.getByTestId("message-input")).not.toContainText("dbl2099");
});

test("an input method's Enter commits its text and never sends", async ({ page }) => {
  await boot(page);
  const text = "dbl2099 composing";
  await typeDraft(page, text);

  // Mid-composition (Chromium, Firefox) and the committing Enter WebKit sends after compositionend.
  await enterInOneTask(page, [{ isComposing: true }, { keyCode: 229 } as KeyboardEventInit]);

  await expect(page.getByTestId("message-input")).toContainText(text);
  expect(await sendCalls(page)).toBe(0);

  // The Enter after the composition is the user's, and sends once.
  await page.keyboard.press("Enter");
  await expect(page.getByText(text)).toHaveCount(1);
  await expect.poll(() => storedCopies(page, text)).toBe(1);
  expect(await sendCalls(page)).toBe(1);
});
