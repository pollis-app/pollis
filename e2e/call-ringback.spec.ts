// Caller ringback (#1901): starting a DM call plays a looping ringback until the call is answered,
// declined, cancelled, times out or the caller hangs up. Never for a group voice channel, and not
// with sound effects off. The tone is Rust's (`start_ringback` / `stop_ringback`); the mock records
// whether it is playing.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const BOB = { id: "u-bob", username: "bob" };
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";
const CALL_ID = "call1901";
const GROUP_ID = "g_voice";
const VOICE_CHANNEL = "c_lounge";

type TestWindow = Window & {
  __tauriMock: { ringback: boolean };
  __tauriInvokeCounts: Record<string, number>;
  __tauriLastArgs: Record<string, Record<string, unknown>>;
  __emitRealtimeEvent: (event: unknown) => number;
  __pollisVoice: { injectEvent: (event: unknown) => void };
};

// Bob accepted the DM, so it can escalate to a call.
function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "", accepted_at: "2026-08-01T09:00:00.000Z" };
}

async function boot(page: Page, prefs: Record<string, unknown> = {}) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: { skin: "terminal", ...prefs },
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
    dmChannels: [
      {
        id: DM_ID,
        created_by: ME.id,
        created_at: new Date().toISOString(),
        members: [member(ME.id, ME.username), member(BOB.id, BOB.username)],
      },
    ],
    messages: {
      [DM_ID]: [
        { id: DM_MSG, conversation_id: DM_ID, sender_id: BOB.id, content: "hi", sent_at: "2026-08-01T10:00:00.000Z" },
      ],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

// Memory-history router: the DM is reached via a permalink in Cmd+K.
async function openDm(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${DM_ID}/${DM_MSG}`);
  await expect(page.getByTestId(`message-${DM_MSG}`)).toBeVisible();
}

/** Call bob from the DM header (once he shows online) and wait for the call screen. */
async function callBob(page: Page) {
  await openDm(page);
  await emit(page, { type: "presence_changed", user_id: BOB.id, room_id: DM_ID, present: true });
  await page.getByTestId("dm-header-call").click();
  await expect(page.getByTestId("call-hang-up")).toBeVisible();
}

function ringback(page: Page): Promise<boolean> {
  return page.evaluate(() => (window as unknown as TestWindow).__tauriMock.ringback);
}

function count(page: Page, command: string): Promise<number> {
  return page.evaluate((c) => (window as unknown as TestWindow).__tauriInvokeCounts[c] ?? 0, command);
}

async function expectRinging(page: Page) {
  await expect.poll(() => ringback(page)).toBe(true);
  expect(await count(page, "start_ringback")).toBe(1);
}

async function expectStopped(page: Page) {
  await expect.poll(() => ringback(page)).toBe(false);
  expect(await count(page, "stop_ringback")).toBe(1);
}

/** Push a realtime event once the inbox channel is open. */
async function emit(page: Page, event: Record<string, unknown>) {
  await expect
    .poll(() => page.evaluate((e) => (window as unknown as TestWindow).__emitRealtimeEvent(e), event))
    .toBe(1);
}

test.describe("caller ringback", () => {
  test("rings back while the call rings; stops when bob answers", async ({ page }) => {
    await boot(page);
    expect(await ringback(page)).toBe(false);
    await callBob(page);
    await expectRinging(page);

    await page.evaluate(
      (e) => (window as unknown as TestWindow).__pollisVoice.injectEvent(e),
      { type: "participant_joined", identity: `voice-${BOB.id}`, name: BOB.username, is_muted: false },
    );
    await expectStopped(page);
    // Answered: the call carries on, and hanging up now is a disconnect, not a ring cancel.
    await expect(page.getByTestId("call-hang-up")).toBeVisible();
  });

  test("stops when bob declines", async ({ page }) => {
    await boot(page);
    await callBob(page);
    await expectRinging(page);

    // A decline reaches the caller as `call_canceled` (incomingCall.ts → cancel_call).
    await emit(page, { type: "call_canceled", call_id: CALL_ID });
    await expectStopped(page);
  });

  test("stops when the call is cancelled by leaving its screen", async ({ page }) => {
    await boot(page);
    await callBob(page);
    await expectRinging(page);

    // Back out of the call screen: its backstop cancels the unanswered call.
    await page.getByTestId("sidebar").getByRole("button", { name: "lounge" }).click();
    await expectStopped(page);
    await expect.poll(() => count(page, "cancel_call")).toBe(1);
  });

  test("stops when the caller hangs up", async ({ page }) => {
    await boot(page);
    await callBob(page);
    await expectRinging(page);

    await page.getByTestId("call-hang-up").click();
    await expectStopped(page);
    await expect.poll(() => count(page, "cancel_call")).toBe(1);
  });

  test("stops when nobody answers within the ring timeout", async ({ page }) => {
    await page.clock.install();
    await boot(page);
    await callBob(page);
    await expectRinging(page);

    // Call.tsx hangs up after 30 s alone.
    await page.clock.fastForward(29_000);
    expect(await ringback(page)).toBe(true);
    await page.clock.fastForward(2_000);
    await expectStopped(page);
    await expect.poll(() => count(page, "cancel_call")).toBe(1);
  });

  test("silent with sound effects off", async ({ page }) => {
    await boot(page, { allow_sound_effects: false });
    await callBob(page);
    await page.waitForTimeout(500);
    expect(await ringback(page)).toBe(false);
    expect(await count(page, "start_ringback")).toBe(0);
  });

  test("never for a group voice channel", async ({ page }) => {
    await boot(page);
    await page.getByTestId("sidebar").getByRole("button", { name: "lounge" }).click();
    await page.getByTestId("voice-join-cta").click();
    await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
    await page.waitForTimeout(500);
    expect(await ringback(page)).toBe(false);
    expect(await count(page, "start_ringback")).toBe(0);
  });
});
