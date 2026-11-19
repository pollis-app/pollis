// The durable outbox (#1335): an accepted message is delivered or visibly unsent, never silently gone.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", username: "bob" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MSG_BOB = "01HQ7Z3K9M2P5R8T1V4W6Y0BOB";
const MSG_UNSENT = "01HQ7Z3K9M2P5R8T1V4W6Y0UNS";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

function preloadState(skin: Skin, unsent: "pending" | "failed", failSends: boolean) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
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
        {
          id: MSG_UNSENT,
          conversation_id: CHANNEL_ID,
          sender_id: USER.id,
          sender_username: USER.username,
          content: "typed just before the app was killed",
          sent_at: "2026-10-14T15:04:00.000Z",
          send_state: unsent,
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

const sendState = (page: Page, id: string) =>
  page.evaluate(
    ([channel, msg]) =>
      (window as any).__tauriMock.messages[channel].find((m: { id: string }) => m.id === msg)
        ?.send_state ?? null,
    [CHANNEL_ID, id],
  );

test("a message a killed process left unsent is delivered by the launch drain", async ({ page }) => {
  await boot(page, preloadState("modern", "pending", false));
  await openChannel(page);
  const row = page.getByTestId(`message-${MSG_UNSENT}`);
  await expect(row).toContainText("typed just before the app was killed");
  await expect.poll(() => sendState(page, MSG_UNSENT)).toBeNull();
  // Delivered: no failure label, nothing to retry.
  await expect(row).not.toContainText("[Not sent]");
  await expect(page.getByTestId(`message-retry-${MSG_UNSENT}`)).toHaveCount(0);
});

for (const skin of SKINS) {
  test(`an unsent message that cannot go is failed, and Retry delivers it (${skin})`, async ({ page }) => {
    await boot(page, preloadState(skin, "pending", true));
    await openChannel(page);
    const row = page.getByTestId(`message-${MSG_UNSENT}`);
    // The launch drain tried and failed: labelled, with its way out.
    await expect(row).toContainText("[Not sent]");
    // Said in plain words (#1668): the outbox will resend it.
    await expect(page.getByTestId(`message-unsent-reason-${MSG_UNSENT}`)).toHaveText(
      "It'll send automatically when Pollis reconnects.",
    );
    await expect(page.getByTestId(`message-retry-${MSG_UNSENT}`)).toBeVisible();
    await expect(page.getByTestId(`message-discard-${MSG_UNSENT}`)).toBeVisible();
    await page.screenshot({ path: test.info().outputPath(`outbox-failed-${skin}.png`) });

    // Back online: Retry delivers it — the same message, once.
    await page.evaluate(() => (window as any).__tauriFailSends(false));
    await page.getByTestId(`message-retry-${MSG_UNSENT}`).click();
    await expect(row).not.toContainText("[Not sent]");
    await expect(page.getByTestId(`message-retry-${MSG_UNSENT}`)).toHaveCount(0);
    expect(await sendState(page, MSG_UNSENT)).toBeNull();
    const copies = await page.evaluate(
      (channel) =>
        (window as any).__tauriMock.messages[channel].filter(
          (m: { content?: string }) => m.content === "typed just before the app was killed",
        ).length,
      CHANNEL_ID,
    );
    expect(copies).toBe(1);
  });
}

test("a send made offline is kept as failed at once, and Retry sends it", async ({ page }) => {
  await boot(page, preloadState("modern", "pending", false));
  await openChannel(page);
  // The OS says offline: no retry can land, so the core does not wait them out (#2055).
  await page.evaluate(() => {
    (window as any).__tauriSetOffline(true);
    (window as any).__tauriFailSends(true);
  });

  await page.getByTestId("message-input").click();
  await page.keyboard.type("sent while offline");
  await page.keyboard.press("Enter");

  const retry = page.locator("[data-testid^='message-retry-']");
  await expect(retry).toHaveCount(1);
  const failedRow = page.locator("[data-testid^='message-']", { hasText: "sent while offline" }).first();
  await expect(failedRow).toContainText("[Not sent]");

  await page.evaluate(() => {
    (window as any).__tauriSetOffline(false);
    (window as any).__tauriFailSends(false);
  });
  await retry.click();
  await expect(page.locator("[data-testid^='message-retry-']")).toHaveCount(0);
  await expect(page.getByText("sent while offline")).toHaveCount(1);
});

/** Flags `window.__sawNotSent` the moment any row shows "Not sent" or a Retry, however briefly. */
async function watchForNotSent(page: Page) {
  await page.evaluate(() => {
    const w = window as unknown as { __sawNotSent: boolean };
    w.__sawNotSent = false;
    const check = () => {
      if (
        document.body.innerText.includes("[Not sent]") ||
        document.querySelector("[data-testid^='message-retry-']")
      ) {
        w.__sawNotSent = true;
      }
    };
    new MutationObserver(check).observe(document.body, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    check();
  });
}

const sawNotSent = (page: Page) =>
  page.evaluate(() => (window as unknown as { __sawNotSent: boolean }).__sawNotSent);

/** The row of our message with `text`, by its stored id. */
const storedId = (page: Page, text: string) =>
  page.evaluate(
    ([channel, content]) =>
      (window as any).__tauriMock.messages[channel].find(
        (m: { content?: string }) => m.content === content,
      )?.id ?? null,
    [CHANNEL_ID, text],
  );

test("a send through a brief network blip stays sending and lands, never Not sent (#2055)", async ({
  page,
}) => {
  const preload = { ...preloadState("modern", "pending", false), outboxRetryDelaysMs: [5_000, 5_000, 5_000] };
  await boot(page, preload);
  await openChannel(page);
  await watchForNotSent(page);
  await page.evaluate(() => (window as any).__tauriFailSends(true));

  await page.getByTestId("message-input").click();
  await page.keyboard.type("sent through a blip");
  await page.keyboard.press("Enter");

  // The first attempt failed; the core keeps it pending and retries it in place.
  await expect.poll(() => storedId(page, "sent through a blip")).not.toBeNull();
  const id = (await storedId(page, "sent through a blip")) as string;
  await expect.poll(() => sendState(page, id)).toBe("pending");
  // While the send is out the row is its optimistic stub, under its own id.
  const row = page.locator("[data-testid^='message-']", { hasText: "sent through a blip" }).first();
  await expect(row).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("outbox-blip-sending.png") });

  // The link comes back before the retries are spent.
  await page.evaluate(() => (window as any).__tauriFailSends(false));
  await expect.poll(() => sendState(page, id), { timeout: 20_000 }).toBeNull();
  await expect(row).not.toContainText("[Not sent]");
  await page.screenshot({ path: test.info().outputPath("outbox-blip-delivered.png") });
  expect(await sawNotSent(page)).toBe(false);
  await expect(page.getByText("sent through a blip")).toHaveCount(1);
});

test("a send through a long outage ends Not sent once the retries are spent (#2055)", async ({ page }) => {
  const preload = { ...preloadState("modern", "pending", false), outboxRetryDelaysMs: [500, 500, 500] };
  await boot(page, preload);
  await openChannel(page);
  await page.evaluate(() => (window as any).__tauriFailSends(true));

  await page.getByTestId("message-input").click();
  await page.keyboard.type("sent into an outage");
  await page.keyboard.press("Enter");

  await expect.poll(() => storedId(page, "sent into an outage")).not.toBeNull();
  const id = (await storedId(page, "sent into an outage")) as string;
  // Still sending while the retries run.
  expect(await sendState(page, id)).toBe("pending");
  await expect(page.getByTestId(`message-retry-${id}`)).toHaveCount(0);

  // Spent: now it says so, with its way out.
  const row = page.getByTestId(`message-${id}`);
  await expect(row).toContainText("[Not sent]", { timeout: 15_000 });
  expect(await sendState(page, id)).toBe("failed");
  await expect(page.getByTestId(`message-retry-${id}`)).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("outbox-outage-not-sent.png") });
});

test("an attachment message whose upload never finished is shown, pending, with Retry", async ({ page }) => {
  // What the core stores before any network work for an attachment send: the
  // files without a `key` (nothing uploaded yet), and the caption.
  const preload = preloadState("modern", "failed", true);
  const unsent = preload.messages[CHANNEL_ID][1] as Record<string, unknown>;
  unsent.content = JSON.stringify({
    _att: [{ name: "shot.png", ct: "image/png", size: 1024 }],
    _txt: "the screenshot",
  });
  const errors: string[] = [];
  // The browser build's event-listener shim throws on teardown in every spec;
  // that noise is not this row's doing.
  page.on("pageerror", (e) => {
    if (!String(e).includes("unregisterListener")) {
      errors.push(String(e));
    }
  });
  await boot(page, preload);
  await openChannel(page);

  const row = page.getByTestId(`message-${MSG_UNSENT}`);
  await expect(row).toContainText("the screenshot");
  await expect(row.getByTestId("attachment-uploading-0")).toBeVisible();
  await expect(row).toContainText("[Not sent]");
  await expect(page.getByTestId(`message-retry-${MSG_UNSENT}`)).toBeVisible();
  expect(errors).toEqual([]);
});

test("a send with attachments hands them to the core un-uploaded", async ({ page }) => {
  await boot(page, preloadState("modern", "pending", false));
  await openChannel(page);
  const src = await page.evaluate(async () => {
    const mod = await import("/src/utils/attachmentEnvelope.ts");
    return mod.outgoingAttachments([
      { id: "a", name: "notes.txt", mimeType: "text/plain", size: 3, source: { kind: "staged", id: "S1" } },
      { id: "b", name: "photo.jpg", mimeType: "image/jpeg", size: 9, source: { kind: "path", path: "/u/photo.jpg" } },
    ] as never);
  });
  expect(src).toEqual([
    { stagedId: "S1", filename: "notes.txt", contentType: "text/plain" },
    { path: "/u/photo.jpg", filename: "photo.jpg", contentType: "image/jpeg" },
  ]);
});

test("Discard removes an unsent message", async ({ page }) => {
  await boot(page, preloadState("terminal", "failed", true));
  await openChannel(page);
  await expect(page.getByTestId(`message-${MSG_UNSENT}`)).toBeVisible();
  await page.getByTestId(`message-discard-${MSG_UNSENT}`).click();
  await expect(page.getByTestId(`message-${MSG_UNSENT}`)).toHaveCount(0);
  const left = await page.evaluate(
    (channel) => (window as any).__tauriMock.messages[channel].map((m: { id: string }) => m.id),
    CHANNEL_ID,
  );
  expect(left).toEqual([MSG_BOB]);
});

const MSG_MINE = "01HQ7Z3K9M2P5R8T1V4W6Y0MNE";

const editState = (page: Page, id: string) =>
  page.evaluate(
    ([channel, msg]) =>
      (window as any).__tauriMock.messages[channel].find((m: { id: string }) => m.id === msg)
        ?.edit_state ?? null,
    [CHANNEL_ID, id],
  );

for (const skin of SKINS) {
  test(`an edit that cannot go is kept, labelled, and the drain delivers it (${skin})`, async ({ page }) => {
    const preload = preloadState(skin, "pending", true);
    preload.messages[CHANNEL_ID].push({
      id: MSG_MINE,
      conversation_id: CHANNEL_ID,
      sender_id: USER.id,
      sender_username: USER.username,
      content: "first draft",
      sent_at: "2026-10-14T15:03:00.000Z",
    });
    await boot(page, preload);
    await openChannel(page);

    const row = page.getByTestId(`message-${MSG_MINE}`);
    await row.hover();
    await row.getByTestId("edit-button").click();
    const input = page.getByTestId("edit-message-bar-input");
    await input.fill("second draft");
    await input.press("Enter");

    // The core kept the edit: the author sees it, marked as not sent.
    await expect(row).toContainText("second draft");
    await expect(row.getByTestId("message-edit-unsent")).toBeVisible();
    expect(await editState(page, MSG_MINE)).toBe("failed");
    await page.screenshot({ path: test.info().outputPath(`outbox-edit-failed-${skin}.png`) });

    // Back online: the drain delivers it, with no user action.
    await page.evaluate(() => (window as any).__tauriFailSends(false));
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect.poll(() => editState(page, MSG_MINE)).toBeNull();
    await expect(row.getByTestId("message-edit-unsent")).toHaveCount(0);
    await expect(row).toContainText("second draft");
  });
}
