// Incoming-call takeover (#1639), both skins: a ringing DM call replaces the screen with a caller-ID
// route, hovering Accept pre-warms the room, and declining / the caller cancelling returns the user to
// exactly where they were. Off in Notifications: no takeover, every other indicator unchanged.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const BOB = { id: "u_bob", username: "bob" };
const GROUP_ID = "g_calls";
const GENERAL = "c_general";
const CALL_ID = "call_1";
const ROOM = `call-${CALL_ID}`;
const DRAFT = "half-written thought";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadFor(skin: Skin, prefs: Record<string, unknown> = {}) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "Book club",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "admin",
      },
    ],
    channels: {
      [GROUP_ID]: [{ id: GENERAL, group_id: GROUP_ID, name: "general" }],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: ME.username, role: "admin", joined_at: "2026-01-01T00:00:00Z" },
        { user_id: BOB.id, username: BOB.username, role: "member", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: { [GENERAL]: [] },
    dmChannels: [],
    preferences: { skin, ...prefs },
  };
}

/** Boot into #general with a draft in the composer: the state the takeover must not cost the user. */
async function bootInChannel(page: Page, skin: Skin, prefs: Record<string, unknown> = {}) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin, prefs));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await openGeneral(page);
  await page.getByTestId("message-input").click();
  await page.getByTestId("message-input").pressSequentially(DRAFT);
  await expect(page.getByTestId("message-input")).toHaveAttribute("data-value", DRAFT);
}

async function openGeneral(page: Page) {
  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${GENERAL}`).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

/** Back to #general from any other screen, through the sidebar (the home menu exists only on Home). */
async function sidebarToGeneral(page: Page) {
  await page.getByTestId("sidebar").getByRole("button", { name: "general", exact: true }).click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

/** Push a realtime event once the inbox channel is open (the mock drops events before it is). */
async function emit(page: Page, event: Record<string, unknown>) {
  await expect
    .poll(() =>
      page.evaluate(
        (e) => (window as unknown as { __emitRealtimeEvent: (x: unknown) => number }).__emitRealtimeEvent(e),
        event,
      ),
    )
    .toBe(1);
}

const ring = (page: Page) =>
  emit(page, {
    type: "call_invite",
    call_id: CALL_ID,
    room_name: ROOM,
    caller_id: BOB.id,
    caller_username: BOB.username,
  });

function count(page: Page, command: string): Promise<number> {
  return page.evaluate(
    (c) => (window as unknown as { __tauriInvokeCounts: Record<string, number> }).__tauriInvokeCounts[c] ?? 0,
    command,
  );
}

function lastArgs(page: Page, command: string): Promise<Record<string, unknown> | null> {
  return page.evaluate(
    (c) =>
      ((window as unknown as { __tauriLastArgs: Record<string, Record<string, unknown>> }).__tauriLastArgs[c] ??
        null),
    command,
  );
}

/** Back in #general with the draft intact: the user lost nothing. */
async function expectReturnedToChannel(page: Page) {
  await expect(page.getByTestId("incoming-call-takeover")).toHaveCount(0);
  await expect(page.getByTestId("message-input")).toBeVisible();
  await expect(page.getByTestId("message-input")).toHaveAttribute("data-value", DRAFT);
}

for (const skin of SKINS) {
  const agnosticTest = skinAgnosticTest(skin);
  test.describe(`incoming-call takeover — ${skin} skin`, () => {
    test("a ringing DM call takes over the screen; the status-bar alert stays", async ({ page }) => {
      await bootInChannel(page, skin);
      await ring(page);

      const takeover = page.getByTestId("incoming-call-takeover");
      await expect(takeover).toBeVisible();
      await expect(page.getByTestId("incoming-call-caller")).toContainText("bob");
      await expect(page.getByTestId("incoming-call-accept")).toBeVisible();
      await expect(page.getByTestId("incoming-call-decline")).toBeVisible();
      // Not an overlay: the chat it replaced is gone from the page, not hidden behind a backdrop.
      await expect(page.getByTestId("message-input")).toHaveCount(0);
      // Every existing indicator is still there.
      await expect(page.getByTestId("status-bar-incoming-call")).toBeVisible();
      await page.screenshot({ path: `artifacts/incoming-call-takeover-${skin}.png` });
    });

    agnosticTest("hovering Accept pre-warms the call room; accepting joins it", async ({ page }) => {
      await bootInChannel(page, skin);
      await ring(page);
      await expect(page.getByTestId("incoming-call-takeover")).toBeVisible();
      await page.evaluate(() => {
        (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
      });

      await page.getByTestId("incoming-call-accept").hover();
      await expect.poll(() => count(page, "prepare_voice_connection")).toBe(1);
      expect((await lastArgs(page, "prepare_voice_connection"))?.channelId).toBe(ROOM);

      await page.getByTestId("incoming-call-accept").click();
      await expect(page.getByTestId("incoming-call-takeover")).toHaveCount(0);
      await expect(page.getByTestId("status-bar-incoming-call")).toHaveCount(0);
      await expect.poll(() => count(page, "join_voice_channel")).toBe(1);
      expect((await lastArgs(page, "join_voice_channel"))?.channelId).toBe(ROOM);
      // Other devices stop ringing; the caller is not told "declined".
      await expect.poll(() => count(page, "dismiss_call_on_my_devices")).toBe(1);
      expect(await count(page, "cancel_call")).toBe(0);
    });

    agnosticTest("declining tells the caller and returns the user where they were", async ({ page }) => {
      await bootInChannel(page, skin);
      await ring(page);
      await expect(page.getByTestId("incoming-call-takeover")).toBeVisible();
      await page.evaluate(() => {
        (window as unknown as { __resetTauriInvokeCounts: () => void }).__resetTauriInvokeCounts();
      });

      await page.getByTestId("incoming-call-decline").click();
      await expectReturnedToChannel(page);
      await expect(page.getByTestId("status-bar-incoming-call")).toHaveCount(0);
      await expect.poll(() => count(page, "cancel_call")).toBe(1);
      expect((await lastArgs(page, "cancel_call"))?.callId).toBe(CALL_ID);
      expect(await count(page, "prepare_voice_connection")).toBe(0);
      expect(await count(page, "join_voice_channel")).toBe(0);
    });

    agnosticTest("the caller cancelling returns the user where they were", async ({ page }) => {
      await bootInChannel(page, skin);
      await ring(page);
      await expect(page.getByTestId("incoming-call-takeover")).toBeVisible();

      await emit(page, { type: "call_canceled", call_id: CALL_ID });
      await expectReturnedToChannel(page);
      await expect(page.getByTestId("status-bar-incoming-call")).toHaveCount(0);
      // Nothing was sent back: the caller already hung up.
      expect(await count(page, "cancel_call")).toBe(0);
    });

    agnosticTest("declining from the status bar also returns the user", async ({ page }) => {
      await bootInChannel(page, skin);
      await ring(page);
      await expect(page.getByTestId("incoming-call-takeover")).toBeVisible();

      await page.getByTestId("status-bar-incoming-call-decline").click();
      await expectReturnedToChannel(page);
    });

    agnosticTest("leaving the takeover keeps the call ringing and never drags the user back", async ({ page }) => {
      await bootInChannel(page, skin);
      await ring(page);
      await expect(page.getByTestId("incoming-call-takeover")).toBeVisible();

      await sidebarToGeneral(page);
      await expect(page.getByTestId("incoming-call-takeover")).toHaveCount(0);
      await expect(page.getByTestId("status-bar-incoming-call")).toBeVisible();
      await page.waitForTimeout(500);
      await expect(page.getByTestId("incoming-call-takeover")).toHaveCount(0);

      // The call ending while the user is elsewhere moves them nowhere.
      await emit(page, { type: "call_canceled", call_id: CALL_ID });
      await expect(page.getByTestId("status-bar-incoming-call")).toHaveCount(0);
      await expect(page.getByTestId("message-input")).toBeVisible();
    });

    agnosticTest("turned off in Notifications: no takeover, the status-bar alert unchanged", async ({ page }) => {
      await bootInChannel(page, skin);

      // Cmd+K to Preferences ("appearance": the Settings hub sorts ahead for "preferences").
      await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await page.getByTestId("search-panel-input").fill("appearance");
      await page.keyboard.press("Enter");
      await expect(page.getByTestId("preferences-page")).toBeVisible();

      const toggle = page.locator("#pref-incoming-call-takeover");
      await expect(toggle).toHaveAttribute("aria-checked", "true");
      await toggle.click();
      await expect(toggle).toHaveAttribute("aria-checked", "false");
      // Persisted in the synced preferences blob, like the other notification settings.
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as unknown as { __tauriMock: { preferences: Record<string, unknown> } }).__tauriMock
                .preferences.incoming_call_takeover,
          ),
        )
        .toBe(false);

      await sidebarToGeneral(page);
      await ring(page);
      const alert = page.getByTestId("status-bar-incoming-call");
      await expect(alert).toBeVisible();
      await expect(page.getByTestId("status-bar-incoming-call-accept")).toContainText("bob");
      await page.waitForTimeout(1000);
      await expect(page.getByTestId("incoming-call-takeover")).toHaveCount(0);
      await expect(page.getByTestId("message-input")).toHaveAttribute("data-value", DRAFT);
    });

    agnosticTest("off from a synced preference: no takeover on launch either", async ({ page }) => {
      await bootInChannel(page, skin, { incoming_call_takeover: false });
      await ring(page);
      await expect(page.getByTestId("status-bar-incoming-call")).toBeVisible();
      await page.waitForTimeout(1000);
      await expect(page.getByTestId("incoming-call-takeover")).toHaveCount(0);
    });
  });
}
