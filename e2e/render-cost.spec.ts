// Message-log + voice-stage render cost (#874): memoised rows and tiles must not re-render needlessly, yet must still update.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1XCB";

// Enough rows that a per-row regression is unmistakable rather than noise.
const ROW_COUNT = 12;
const OWN_ID = "01HQ7Z3K9M2P5R8T1V4W6Y10011";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

function messages() {
  const out = [];
  for (let i = 0; i < ROW_COUNT; i++) {
    const own = i === ROW_COUNT - 1;
    out.push({
      id: own ? OWN_ID : `01HQ7Z3K9M2P5R8T1V4W6Y1${String(i).padStart(3, "0")}`,
      conversation_id: CHANNEL_ID,
      sender_id: own ? USER.id : "u-bob",
      content: own ? "my own newest message" : `message number ${i}`,
      // All on one day, so the rows render without a day boundary between them.
      sent_at: `2026-08-01T10:${String(i).padStart(2, "0")}:00.000Z`,
    });
  }
  return out;
}

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: USER.id,
        created_at: new Date().toISOString(),
      },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    dmChannels: [],
    messages: { [CHANNEL_ID]: messages() },
    bookmarks: [],
  };
}

async function boot(page: Page, skin: Skin) {
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
  }, preloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function gotoChannel(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${OWN_ID}`)).toBeVisible();
}

/** Total render-function entries for a probed component since page load. */
async function renders(page: Page, name: string): Promise<number> {
  return await page.evaluate((probe) => {
    const w = window as unknown as { __pollisRenders?: Record<string, number> };
    return w.__pollisRenders?.[probe] ?? 0;
  }, name);
}

/** The probe must exist, or every "no new renders" assertion passes vacuously. */
async function assertProbeLive(page: Page) {
  expect(await renders(page, "MessageItem")).toBeGreaterThan(0);
  expect(await renders(page, "MessageList")).toBeGreaterThan(0);
}

/** Wait for the windowed log's post-layout re-render to settle before taking a baseline. */
async function settleRenders(page: Page) {
  let previous = -1;
  let stableFor = 0;
  await expect
    .poll(
      async () => {
        const current = await renders(page, "MessageList");
        stableFor = current === previous ? stableFor + 1 : 0;
        previous = current;
        // Three quiet polls in a row (~300ms): the list's post-mount scroll
        // settle lands a couple of hundred milliseconds after first paint, and
        // a single matching pair can fall on either side of it.
        return stableFor >= 3;
      },
      { intervals: [100, 100, 100, 100, 200, 200, 400] },
    )
    .toBe(true);
}

for (const skin of SKINS) {
  test.describe(`message log render cost — ${skin} skin`, () => {
    test("typing in the edit bar re-renders no message rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);

      const own = page.getByTestId(`message-${OWN_ID}`);
      await own.hover();
      await own.getByTestId("edit-button").click();
      const editInput = page.getByTestId("edit-message-bar-input");
      await expect(editInput).toBeVisible();

      // Baseline taken AFTER the bar is open: opening it is a legitimate state
      // change. What must cost nothing is the typing that follows.
      await settleRenders(page);
      const rowsBefore = await renders(page, "MessageItem");
      const listBefore = await renders(page, "MessageList");

      await editInput.click();
      await page.keyboard.type("edited text here", { delay: 10 });
      await expect(editInput).toHaveValue(/edited text here$/);

      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
      expect(await renders(page, "MessageList")).toBe(listBefore);
    });

    test("opening the reply bar re-renders no message rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);
      await settleRenders(page);

      const rowsBefore = await renders(page, "MessageItem");

      const own = page.getByTestId(`message-${OWN_ID}`);
      await own.hover();
      await own.getByTestId("reply-button").click();
      await expect(page.getByTestId("message-input")).toBeFocused();

      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
    });

    test("arrow-key log navigation re-renders no message rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);

      await page.getByTestId("message-input").click();
      await settleRenders(page);
      const rowsBefore = await renders(page, "MessageItem");

      // Rows style themselves via CSS :focus-within, so walking the log is
      // supposed to be free. Pin that.
      await page.keyboard.press("ArrowUp");
      await expect(page.getByTestId(`message-${OWN_ID}`)).toBeFocused();
      await page.keyboard.press("ArrowUp");
      await page.keyboard.press("ArrowUp");

      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
    });

    test("an open channel that nobody touches renders nothing", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);
      await settleRenders(page);

      // Not a settle: a fixed idle window in which nothing may render (an effect loop once re-rendered constantly).
      const before = await Promise.all(
        ["AppShell", "MainContent", "MessageList", "MessageItem", "ChatInput"].map((p) =>
          renders(page, p),
        ),
      );
      await page.waitForTimeout(1000);
      const after = await Promise.all(
        ["AppShell", "MainContent", "MessageList", "MessageItem", "ChatInput"].map((p) =>
          renders(page, p),
        ),
      );
      expect(after).toEqual(before);
    });

    test("typing in the composer re-renders only the composer", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);

      const input = page.getByTestId("message-input");
      await input.click();
      await settleRenders(page);
      const shellBefore = await renders(page, "AppShell");
      const contentBefore = await renders(page, "MainContent");
      const listBefore = await renders(page, "MessageList");
      const rowsBefore = await renders(page, "MessageItem");
      const composerBefore = await renders(page, "ChatInput");

      await page.keyboard.type("typing must stay local", { delay: 5 });
      await expect(input).toHaveText(/typing must stay local$/);

      // The composer rendered — otherwise the zeros below prove nothing.
      expect(await renders(page, "ChatInput")).toBeGreaterThan(composerBefore);
      expect(await renders(page, "AppShell")).toBe(shellBefore);
      expect(await renders(page, "MainContent")).toBe(contentBefore);
      expect(await renders(page, "MessageList")).toBe(listBefore);
      expect(await renders(page, "MessageItem")).toBe(rowsBefore);
    });

    test("an AppShell re-render does not re-render the sidebar", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);
      await assertProbeLive(page);
      await settleRenders(page);

      const shellBefore = await renders(page, "AppShell");
      const sidebarBefore = await renders(page, "Sidebar");

      // Cmd+K flips AppShell's own state; the memoised sidebar must absorb it.
      await page.keyboard.press(
        process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
      );
      await expect(page.getByTestId("search-panel")).toBeVisible();

      // The shell really did re-render — otherwise this proves nothing.
      expect(await renders(page, "AppShell")).toBeGreaterThan(shellBefore);
      expect(await renders(page, "Sidebar")).toBe(sidebarBefore);
    });

    test("flipping the skin restructures the rows", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      // Modern rows are a two-column grid; the memoised row must not swallow a skin change.
      const rowDisplay = async () =>
        await page
          .getByTestId(`message-${OWN_ID}`)
          .evaluate((el) => getComputedStyle(el).display);

      expect(await rowDisplay()).toBe(skin === "modern" ? "grid" : "block");

      const other = skin === "terminal" ? "modern" : "terminal";
      // Navigate via the sidebar's testid (page.goto would re-serve the SPA root).
      await page.getByTestId("sidebar-row-preferences").click();
      await page.getByTestId(`pref-skin-${other}`).click();
      await expect(page.locator("html")).toHaveAttribute("data-skin", other);

      await gotoChannel(page);
      await expect(page.locator("html")).toHaveAttribute("data-skin", other);
      expect(await rowDisplay()).toBe(other === "modern" ? "grid" : "block");
    });

    test("editing a message updates that row's text", async ({ page }) => {
      await boot(page, skin);
      await gotoChannel(page);

      const own = page.getByTestId(`message-${OWN_ID}`);
      await expect(own).toContainText("my own newest message");

      await own.hover();
      await own.getByTestId("edit-button").click();
      const editInput = page.getByTestId("edit-message-bar-input");
      await expect(editInput).toBeVisible();
      await editInput.fill("the edited body");
      await page.keyboard.press("Enter");

      // The memoised row must reflect the new content.
      await expect(own).toContainText("the edited body");
    });
  });
}

// ---- Incoming message into a long log (#2167) --------------------------------

// More rows than one page (50), so an arrival slides the page window the way the core's read does.
const LONG_ROW_COUNT = 60;
const INCOMING_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1INC1";
const longId = (i: number) => `01HQ7Z3K9M2P5R8T1V4W6Y1${String(i).padStart(3, "0")}`;
// The newest row is our own, so its send state can change under the open log.
const LONG_OWN_ID = longId(LONG_ROW_COUNT - 1);

function longMessages(ownSendState?: "pending" | "failed") {
  const out = [];
  for (let i = 0; i < LONG_ROW_COUNT; i++) {
    const own = i === LONG_ROW_COUNT - 1;
    out.push({
      id: longId(i),
      conversation_id: CHANNEL_ID,
      sender_id: own ? USER.id : "u-bob",
      content: own ? "my own pending message" : `long-log message ${i}`,
      sent_at: `2026-08-01T10:${String(i).padStart(2, "0")}:00.000Z`,
      ...(own && ownSendState ? { send_state: ownSendState } : {}),
    });
  }
  return out;
}

async function bootLongLog(page: Page, ownSendState?: "pending" | "failed") {
  // The core reads the newest 50: the paged mock reproduces the sliding window.
  const preload = { ...preloadState("modern"), paginate: true, messages: { [CHANNEL_ID]: longMessages(ownSendState) } };
  await page.addInitScript((p) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = p;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
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
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await page.getByTestId("search-panel-input").fill("general");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`message-${LONG_OWN_ID}`)).toBeVisible();
  await assertProbeLive(page);
  await settleRenders(page);
}

/** Change the mock store, then invalidate the open page as a message wake does. */
async function mutateAndInvalidate(page: Page, change: "arrival" | "fail" | "retry") {
  await page.evaluate(
    ({ channelId, incomingId, ownId, change }) => {
      const w = window as unknown as {
        __tauriMock: { messages: Record<string, Array<Record<string, unknown>>> };
        __pollisQueryClient: { invalidateQueries: (f: unknown) => Promise<void> };
      };
      const rows = w.__tauriMock.messages[channelId];
      if (change === "arrival") {
        rows.push({
          id: incomingId,
          conversation_id: channelId,
          sender_id: "u-bob",
          content: "a fresh arrival",
          sent_at: "2026-08-01T11:00:00.000Z",
        });
      } else {
        const own = rows.find((r) => r.id === ownId)!;
        own.send_state = change === "fail" ? "failed" : "pending";
      }
      void w.__pollisQueryClient.invalidateQueries({ queryKey: ["messages", "channel", channelId] });
    },
    { channelId: CHANNEL_ID, incomingId: INCOMING_ID, ownId: LONG_OWN_ID, change },
  );
}

test.describe("long log render cost (#2167)", () => {
  test("an arrival plus a log invalidation re-renders only the new row", async ({ page }) => {
    await bootLongLog(page);
    const mounted = await page.locator('[data-testid^="message-01HQ"]').count();
    const rowsBefore = await renders(page, "MessageItem");

    await mutateAndInvalidate(page, "arrival");
    await expect(page.getByTestId(`message-${INCOMING_ID}`)).toBeVisible();
    await settleRenders(page);

    const delta = (await renders(page, "MessageItem")) - rowsBefore;
    console.log(`[render-cost] mounted_rows=${mounted} MessageItem_delta=${delta}`);
    // Only the new row: it commits twice (mount, then its follow-up data), each doubled by StrictMode.
    // Re-rendering every mounted row costs ~2 per row (about 50 here).
    expect(delta).toBeLessThanOrEqual(4);
  });

  test("a send that turns failed repaints its row with Retry, and a retry repaints it again", async ({ page }) => {
    await bootLongLog(page, "pending");
    const own = page.getByTestId(`message-${LONG_OWN_ID}`);
    await expect(page.getByTestId(`message-retry-${LONG_OWN_ID}`)).toHaveCount(0);
    const rowsBefore = await renders(page, "MessageItem");

    // pending -> failed: the memoised row must not keep its "sending" render.
    await mutateAndInvalidate(page, "fail");
    await expect(page.getByTestId(`message-retry-${LONG_OWN_ID}`)).toBeVisible();
    await expect(own).toContainText("my own pending message");
    await settleRenders(page);
    const afterFail = await renders(page, "MessageItem");
    expect(afterFail).toBeGreaterThan(rowsBefore);
    // Only that row repainted, not the log.
    expect(afterFail - rowsBefore).toBeLessThanOrEqual(4);

    // failed -> pending (a retry elsewhere): Retry goes away again.
    await mutateAndInvalidate(page, "retry");
    await expect(page.getByTestId(`message-retry-${LONG_OWN_ID}`)).toHaveCount(0);
    await settleRenders(page);
    expect(await renders(page, "MessageItem")).toBeGreaterThan(afterFail);
  });
});

// ---- Voice stage ------------------------------------------------------------

const VOICE_GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1VGR";
const VOICE_CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y1VCH";
const VOICE_CHANNEL_NAME = "standup";
const REMOTE = ["voice-u-bob", "voice-u-carol", "voice-u-dave"] as const;

function voicePreloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [{ id: VOICE_GROUP_ID, name: "Acme", owner_id: USER.id, created_at: new Date().toISOString() }],
    channels: {
      [VOICE_GROUP_ID]: [
        { id: CHANNEL_ID, group_id: VOICE_GROUP_ID, name: "general", channel_type: "text" },
        { id: VOICE_CHANNEL_ID, group_id: VOICE_GROUP_ID, name: VOICE_CHANNEL_NAME, channel_type: "voice" },
      ],
    },
    dmChannels: [],
    messages: {},
    bookmarks: [],
  };
}

type VoiceWindow = Window & { __pollisVoice: { injectEvent: (event: unknown) => void } };

async function injectVoice(page: Page, event: Record<string, unknown>) {
  await page.evaluate((e) => (window as unknown as VoiceWindow).__pollisVoice.injectEvent(e), event);
}

/** Boot straight into a joined voice channel with three remote participants on the stage. */
async function bootVoiceStage(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
      plugins: {},
      transformCallback: () => 0,
      convertFileSrc: (path: string) => path,
      registerListener: () => {},
      unregisterListener: () => {},
      runCallback: () => {},
      invoke: () => Promise.resolve(null),
    };
  }, voicePreloadState(skin));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await page.getByTestId("sidebar").getByRole("button", { name: VOICE_CHANNEL_NAME }).click();
  await page.getByTestId("voice-join-cta").click();
  await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
  for (const identity of REMOTE) {
    await injectVoice(page, { type: "participant_joined", identity, name: identity.slice("voice-u-".length), is_muted: false });
  }
  for (const identity of REMOTE) {
    await expect(page.getByTestId(`voice-tile-${identity}`)).toBeVisible();
  }
  expect(await renders(page, "StageTile")).toBeGreaterThan(0);
}

/** Wait until no tile has rendered for three quiet polls. */
async function settleStage(page: Page) {
  let previous = -1;
  let stableFor = 0;
  await expect
    .poll(
      async () => {
        const current = await renders(page, "StageTile");
        stableFor = current === previous ? stableFor + 1 : 0;
        previous = current;
        return stableFor >= 3;
      },
      { intervals: [100, 100, 100, 100, 200, 200, 400] },
    )
    .toBe(true);
}

// The stage is the same in both skins; run it once.
const stageTest = skinAgnosticTest("terminal");

stageTest.describe("voice stage render cost", () => {
  /** Every StageTile render since `before` belongs to `tileKey`: the other tiles rendered zero times. */
  async function expectOnlyTileRendered(page: Page, before: Record<string, number>, tileKey: string) {
    const all = (await renders(page, "StageTile")) - (before["StageTile"] ?? 0);
    const one = (await renders(page, `StageTile:${tileKey}`)) - (before[`StageTile:${tileKey}`] ?? 0);
    expect(one).toBeGreaterThan(0);
    expect(all).toBe(one);
    for (const other of [`voice-${USER.id}`, ...REMOTE].filter((k) => k !== tileKey)) {
      expect((await renders(page, `StageTile:${other}`)) - (before[`StageTile:${other}`] ?? 0)).toBe(0);
    }
  }

  async function tileCounts(page: Page): Promise<Record<string, number>> {
    return page.evaluate(() => ({ ...((window as unknown as { __pollisRenders?: Record<string, number> }).__pollisRenders ?? {}) }));
  }

  stageTest("a speaking event re-renders one tile", async ({ page }) => {
    await bootVoiceStage(page, "terminal");
    await settleStage(page);
    const before = await tileCounts(page);
    const shellBefore = await renders(page, "AppShell");

    await injectVoice(page, { type: "speaking_started", identity: REMOTE[0] });
    await expect(page.getByTestId(`voice-tile-${REMOTE[0]}`)).toHaveClass(/speaking/);

    await expectOnlyTileRendered(page, before, REMOTE[0]);
    // The shell must not wake either: the store mirror writes only what changed.
    expect(await renders(page, "AppShell")).toBe(shellBefore);
  });

  stageTest("a remote quality change re-renders only that tile", async ({ page }) => {
    await bootVoiceStage(page, "terminal");
    await settleStage(page);
    const before = await tileCounts(page);

    await injectVoice(page, { type: "connection_quality_changed", identity: REMOTE[1], quality: "poor" });
    await expect(page.getByTestId(`voice-tile-quality-${REMOTE[1]}`)).toHaveAttribute("data-quality", "poor");

    await expectOnlyTileRendered(page, before, REMOTE[1]);
  });

  stageTest("an idle stage renders nothing", async ({ page }) => {
    await bootVoiceStage(page, "terminal");
    await settleStage(page);
    const before = await renders(page, "StageTile");
    await page.waitForTimeout(1000);
    expect(await renders(page, "StageTile")).toBe(before);
  });

  stageTest("a mute click re-renders the local tile only", async ({ page }) => {
    await bootVoiceStage(page, "terminal");
    await settleStage(page);
    const before = await tileCounts(page);

    await page.getByTestId("voice-tray-mute").click();
    await expect(page.getByTestId("voice-tray-mute")).toHaveAttribute("data-mic-state", "muted");
    await settleStage(page);

    // Optimistic paint, then Rust's identical snapshot: only the local tile is touched.
    await expectOnlyTileRendered(page, before, `voice-${USER.id}`);
  });
});
