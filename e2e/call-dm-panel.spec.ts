// A 1:1 call shows its DM in the right panel (#2116): the real DM view with its history beside the
// call; a send from it is a normal DM send; a message arriving while it is visible is read, not
// badged or alerted; the panel's open/closed choice is still the user's; a group voice channel
// never gets one. `CALL_DM_SHOTS=<dir>` writes review screenshots.

import { test, expect, type Page } from "./lib/zero-loss";
import { intersects, rectOf } from "./lib/rects";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", username: "bob" };
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";
const GROUP_DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GDM";
const GROUP_DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0GD1";
const CALL_ID = "call2116";
const GROUP_ID = "g_voice";
const VOICE_CHANNEL = "c_lounge";

const SKINS = ["modern", "terminal"] as const;
type Skin = (typeof SKINS)[number];

const TOGGLE = "Control+Shift+B";
const SHOTS = process.env.CALL_DM_SHOTS;

type TestWindow = Window & {
  __tauriMock: { messages: Record<string, Array<Record<string, unknown>>> };
  __tauriInvokeCounts: Record<string, number>;
  __emitRealtimeEvent: (event: unknown) => number;
  __pollisVoice: { injectEvent: (event: unknown) => void };
};

// Bob accepted the DM, so it can escalate to a call.
function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "", accepted_at: "2026-08-01T09:00:00.000Z" };
}

async function boot(page: Page, skin: Skin, opts: { withOneOnOne?: boolean } = {}) {
  const withOneOnOne = opts.withOneOnOne ?? true;
  const dmChannels = [
    // A group DM with bob in it: never the call's DM.
    {
      id: GROUP_DM_ID,
      created_by: ME.id,
      created_at: "2026-07-01T09:00:00.000Z",
      members: [member(ME.id, ME.username), member(BOB.id, BOB.username), member("u-carol", "carol")],
    },
  ];
  if (withOneOnOne) {
    dmChannels.push({
      id: DM_ID,
      created_by: ME.id,
      created_at: "2026-08-01T09:00:00.000Z",
      members: [member(ME.id, ME.username), member(BOB.id, BOB.username)],
    });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    // The panel starts open on every skin; the user's own toggling is what persists.
    preferences: { skin, right_panel_open_by_default: true, allow_desktop_notifications: true },
    notificationPermission: true,
    nextCallId: CALL_ID,
    groups: [
      { id: GROUP_ID, name: "voice", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: {
      [GROUP_ID]: [{ id: VOICE_CHANNEL, group_id: GROUP_ID, name: "lounge", channel_type: "voice" }],
    },
    groupMembers: {
      [GROUP_ID]: [{ user_id: ME.id, username: ME.username, role: "admin", joined_at: "2026-01-01T00:00:00Z" }],
    },
    dmChannels,
    messages: {
      [DM_ID]: [
        { id: DM_MSG, conversation_id: DM_ID, sender_id: BOB.id, sender_username: BOB.username, content: "want to hop on a call?", sent_at: "2026-08-01T10:00:00.000Z" },
      ],
      [GROUP_DM_ID]: [
        { id: GROUP_DM_MSG, conversation_id: GROUP_DM_ID, sender_id: BOB.id, sender_username: BOB.username, content: "group hello", sent_at: "2026-07-01T10:00:00.000Z" },
      ],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

/** Push a realtime event once the inbox channel is open. */
async function emit(page: Page, event: Record<string, unknown>) {
  await expect
    .poll(() => page.evaluate((e) => (window as unknown as TestWindow).__emitRealtimeEvent(e), event))
    .toBe(1);
}

// Memory-history router: a conversation is reached via a permalink in Cmd+K.
async function openConversation(page: Page, dmId: string, msgId: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${dmId}/${msgId}`);
  await expect(page.getByTestId(`message-${msgId}`)).toBeVisible();
}

/** Call bob from the 1:1 DM header and let him answer. */
async function callBob(page: Page) {
  await openConversation(page, DM_ID, DM_MSG);
  await emit(page, { type: "presence_changed", user_id: BOB.id, room_id: DM_ID, present: true });
  await page.getByTestId("dm-header-call").click();
  await expect(page.getByTestId("call-hang-up")).toBeVisible();
  await page.evaluate(
    (e) => (window as unknown as TestWindow).__pollisVoice.injectEvent(e),
    { type: "participant_joined", identity: `voice-${BOB.id}`, name: BOB.username, is_muted: false },
  );
}

const panel = (page: Page) => page.getByTestId("right-panel");
const panelDm = (page: Page) => page.getByTestId("right-panel-dm");

function count(page: Page, command: string): Promise<number> {
  return page.evaluate((c) => (window as unknown as TestWindow).__tauriInvokeCounts[c] ?? 0, command);
}

async function resetCounts(page: Page) {
  await page.evaluate(() => {
    (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
  });
}

async function bobSays(page: Page, id: string, content: string) {
  await page.evaluate(
    ({ dm, id, content }) => {
      (window as unknown as TestWindow).__tauriMock.messages[dm].push({
        id,
        conversation_id: dm,
        sender_id: "u-bob",
        sender_username: "bob",
        content,
        sent_at: new Date().toISOString(),
      });
    },
    { dm: DM_ID, id, content },
  );
  await emit(page, { type: "new_message", channel_id: null, conversation_id: DM_ID });
}

async function shot(page: Page, name: string) {
  if (SHOTS) {
    await page.screenshot({ path: `${SHOTS}/${name}.png` });
  }
}

for (const skin of SKINS) {
  test.describe(`DM call panel (${skin})`, () => {
    test("a DM call shows that DM, with its history, in the right panel", async ({ page }) => {
      await boot(page, skin);
      await callBob(page);

      await expect(panel(page)).toBeVisible();
      await expect(panelDm(page).getByTestId(`message-${DM_MSG}`)).toContainText("want to hop on a call?");
      // The group DM bob is also in is not the one shown.
      await expect(page.getByTestId(`message-${GROUP_DM_MSG}`)).toHaveCount(0);
      await expect(page.getByTestId("right-panel-close")).toContainText("Chat");
      // The call stays on screen beside it.
      await expect(page.getByTestId("call-hang-up")).toBeVisible();
      await shot(page, `call-dm-panel-${skin}`);
    });

    test("a message sent from the panel is a send into that DM", async ({ page }) => {
      await boot(page, skin);
      await callBob(page);
      await expect(panelDm(page)).toBeVisible();
      await resetCounts(page);

      await panelDm(page).getByTestId("message-input").click();
      await page.keyboard.type("can you hear me?");
      await page.keyboard.press("Enter");

      await expect(panelDm(page).getByText("can you hear me?")).toBeVisible();
      await expect.poll(() => count(page, "send_message")).toBe(1);
      // What the core stored is bob's to fetch: in the 1:1 DM, from alice.
      const stored = await page.evaluate(
        (dm) =>
          (window as unknown as TestWindow).__tauriMock.messages[dm]
            .filter((m) => m.content === "can you hear me?")
            .map((m) => ({ conversation_id: m.conversation_id, sender_id: m.sender_id })),
        DM_ID,
      );
      expect(stored).toEqual([{ conversation_id: DM_ID, sender_id: ME.id }]);
      await shot(page, `call-dm-panel-sent-${skin}`);
    });

    test("a message arriving while the panel shows the DM is read, not badged or alerted", async ({ page }) => {
      await boot(page, skin);
      await callBob(page);
      await expect(panelDm(page)).toBeVisible();
      await resetCounts(page);

      await bobSays(page, "01HQ7Z3K9M2P5R8T1V4W6Y0DM2", "you're breaking up");
      await expect(panelDm(page).getByTestId("message-01HQ7Z3K9M2P5R8T1V4W6Y0DM2")).toBeVisible();
      await expect.poll(() => count(page, "mark_conversation_read")).toBeGreaterThan(0);
      // Let the async wake run to the end, if it would alert.
      await page.waitForTimeout(500);
      await expect(page.getByTestId(`sidebar-unread-dm-${DM_ID}`)).toHaveCount(0);
      await expect(page.getByTestId("sidebar-unread-dms")).toHaveCount(0);
      expect(await count(page, "plugin:notification|notify")).toBe(0);
    });

    test("with the panel closed the same message badges: the panel is what reads it", async ({ page }) => {
      await boot(page, skin);
      await callBob(page);
      await page.getByTestId("right-panel-close").click();
      await expect(panel(page)).toHaveCount(0);

      await bobSays(page, "01HQ7Z3K9M2P5R8T1V4W6Y0DM3", "hello?");
      await expect(page.getByTestId(`sidebar-unread-dm-${DM_ID}`)).toHaveText("1");
    });

    test("closing and reopening the panel is the user's choice, and it sticks", async ({ page }) => {
      await boot(page, skin);
      await callBob(page);
      await expect(panelDm(page)).toBeVisible();

      await page.getByTestId("right-panel-close").click();
      await expect(panel(page)).toHaveCount(0);

      // Away to the DM and back to the call: still closed.
      await page.getByTestId(`sidebar-row-dm-${DM_ID}`).click();
      await expect(page.getByTestId("dm-header-return-to-call")).toBeVisible();
      await expect(panel(page)).toHaveCount(0);
      await page.getByTestId("dm-header-return-to-call").click();
      await expect(page.getByTestId("call-hang-up")).toBeVisible();
      await expect(panel(page)).toHaveCount(0);

      // The toggle reopens it on the DM; that sticks across the same trip.
      await page.keyboard.press(TOGGLE);
      await expect(panelDm(page).getByTestId(`message-${DM_MSG}`)).toBeVisible();
      await page.getByTestId(`sidebar-row-dm-${DM_ID}`).click();
      await expect(page.getByTestId("dm-header-return-to-call")).toBeVisible();
      await page.getByTestId("dm-header-return-to-call").click();
      await expect(panelDm(page).getByTestId(`message-${DM_MSG}`)).toBeVisible();
    });

    test("a call with someone you have no 1:1 DM with shows no DM panel", async ({ page }) => {
      await boot(page, skin, { withOneOnOne: false });
      // Bob rings in; the only DM he shares is the group one.
      await openConversation(page, GROUP_DM_ID, GROUP_DM_MSG);
      await page.evaluate(
        ({ callId, bob }) => {
          const store = (window as unknown as { __pollisStore: Record<string, Function> }).__pollisStore;
          store.voiceStartJoining(`call-${callId}`, bob);
          store.voiceJoined();
        },
        { callId: CALL_ID, bob: BOB.id },
      );
      await page.getByTestId("dm-header-return-to-call").click();
      await expect(page.getByTestId("call-hang-up")).toBeVisible();
      await expect(panel(page)).toBeVisible();
      await expect(panelDm(page)).toHaveCount(0);
      await expect(page.getByTestId(`message-${GROUP_DM_MSG}`)).toHaveCount(0);
    });

    test("in the narrow panel, a message's hover actions stay clear of its text (#2144)", async ({ page }) => {
      await boot(page, skin);
      await callBob(page);
      await expect(panelDm(page)).toBeVisible();
      const id = "01HQ7Z3K9M2P5R8T1V4W6Y0DM3";
      await bobSays(page, id, "the panel is narrow, so this line wraps a few times beside the call while we talk it over");
      const row = panelDm(page).getByTestId(`message-${id}`);
      await row.getByTestId("message-content").hover();
      const actions = row.getByTestId("message-actions");
      await expect(actions).toHaveCSS("opacity", "1");
      const box = await rectOf(actions);
      // The first painted line of the text (with Terminal's name and ":" sharing it).
      const line = await row.getByTestId("message-content").evaluate((el) => {
        const flow = el.parentElement!.querySelector(':scope > [data-testid="message-author"]') ? el.parentElement! : el;
        const range = document.createRange();
        range.selectNodeContents(flow);
        const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0 && r.height < 40);
        const top = Math.min(...rects.map((r) => r.top));
        const first = rects.filter((r) => r.top < top + 4);
        return {
          left: Math.min(...first.map((r) => r.left)),
          right: Math.max(...first.map((r) => r.right)),
          top,
          bottom: Math.max(...first.map((r) => r.bottom)),
        };
      });
      expect(intersects(box, line), `actions ${JSON.stringify(box)} vs line ${JSON.stringify(line)}`).toBe(false);
      await shot(page, `call-dm-panel-actions-${skin}`);
    });

    test("a group voice channel shows no DM panel", async ({ page }) => {
      await boot(page, skin);
      await page.getByTestId("sidebar").getByRole("button", { name: "lounge" }).click();
      await page.getByTestId("voice-join-cta").click();
      await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
      await expect(panel(page)).toBeVisible();
      await expect(panelDm(page)).toHaveCount(0);
      await expect(page.getByTestId(`message-${DM_MSG}`)).toHaveCount(0);
    });
  });
}
