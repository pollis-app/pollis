// Voice UI feel, BOTH skins: controls flip on the click frame (Rust stays authoritative), the tray
// mounts with the join and Leave works while connecting, a reconnect is shown rather than dropped
// to idle, a lost connection names itself, and connection quality starts unmeasured.
// No LiveKit here: Rust-shaped `VoiceEvent`s are fed through `window.__pollisVoice.injectEvent`.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_voice";
const TEXT_CHANNEL_ID = "c_general";
const VOICE_CHANNEL_ID = "c_lounge";
const VOICE_CHANNEL_NAME = "lounge";
// The mock answers `get_device_id` with null, so the local identity is user-scoped.
const LOCAL_IDENTITY = `voice-${ME.id}`;
const BOB_IDENTITY = "voice-u_bob";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

/** Where each skin keeps its always-present voice strip. */
const STRIP = {
  terminal: { reconnecting: "voice-bar-reconnecting", quality: "voice-bar-quality", mic: "voice-bar-mute-button" },
  refined: { reconnecting: "sidebar-voice-reconnecting", quality: "sidebar-voice-quality", mic: "sidebar-voice-mute" },
} as const;

type TestWindow = Window & {
  __pollisVoice: { injectEvent: (event: unknown) => void };
  __tauriHold: (cmd: string) => void;
  __tauriRelease: (cmd: string) => void;
  __tauriInvokeCounts: Record<string, number>;
  __pollisStore: Record<string, Function>;
};

function preloadFor(skin: Skin) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "voice", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: {
      [GROUP_ID]: [
        { id: TEXT_CHANNEL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" },
        { id: VOICE_CHANNEL_ID, group_id: GROUP_ID, name: VOICE_CHANNEL_NAME, channel_type: "voice" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [{ user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" }],
    },
    messages: {},
    dmChannels: [],
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
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
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
}

async function openVoiceChannel(page: Page) {
  await page.getByTestId("sidebar").getByRole("button", { name: VOICE_CHANNEL_NAME }).click();
  await expect(page.getByTestId("voice-join-cta")).toBeVisible();
}

/** Join through the real `VoiceSessionManager` (the mock's `join_voice_channel` succeeds instantly). */
async function joinVoice(page: Page) {
  await page.getByTestId("voice-join-cta").click();
  await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
}

async function inject(page: Page, event: Record<string, unknown>) {
  await page.evaluate((e) => (window as unknown as TestWindow).__pollisVoice.injectEvent(e), event);
}

async function hold(page: Page, cmd: string) {
  await page.evaluate((c) => (window as unknown as TestWindow).__tauriHold(c), cmd);
}

async function release(page: Page, cmd: string) {
  await page.evaluate((c) => (window as unknown as TestWindow).__tauriRelease(c), cmd);
}

async function invokeCount(page: Page, cmd: string): Promise<number> {
  return page.evaluate((c) => (window as unknown as TestWindow).__tauriInvokeCounts[c] ?? 0, cmd);
}

for (const skin of SKINS) {
  const strip = STRIP[skin];

  test.describe(`voice feel — ${skin} skin`, () => {
    test("mute flips on the click frame, before Rust answers, and a second click can't double-toggle", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      const mute = page.getByTestId("voice-tray-mute");
      await expect(mute).toHaveAttribute("data-mic-state", "live");

      // Park the reply: the only way the control can read "muted" now is the optimistic path.
      await hold(page, "toggle_voice_mute");
      await mute.click();
      await expect(mute).toHaveAttribute("data-mic-state", "muted");
      await expect(mute).toHaveAttribute("aria-pressed", "true");
      await expect(page.getByTestId(strip.mic)).toHaveAttribute("data-mic-state", "muted");
      // The local tile follows the same frame.
      await expect(page.getByTestId(`voice-tile-${LOCAL_IDENTITY}`).locator(".vs-ic.danger")).toBeVisible();

      // A rapid second click while the first is in flight is dropped, not queued into a flip-back.
      await mute.click();
      await mute.click();
      expect(await invokeCount(page, "toggle_voice_mute")).toBe(1);
      await expect(mute).toHaveAttribute("data-mic-state", "muted");

      // Rust answers: it agrees, nothing jumps.
      await release(page, "toggle_voice_mute");
      await expect(mute).toHaveAttribute("data-mic-state", "muted");
      expect(await invokeCount(page, "toggle_voice_mute")).toBe(1);

      // With the first settled, the next click is honoured.
      await mute.click();
      await expect(mute).toHaveAttribute("data-mic-state", "live");
      await expect.poll(() => invokeCount(page, "toggle_voice_mute")).toBe(2);
    });

    test("deafen predicts its mute and the undeafen restores what was there", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      const mute = page.getByTestId("voice-tray-mute");
      const deafen = page.getByTestId("voice-tray-deafen");

      await hold(page, "toggle_voice_deafen");
      await deafen.click();
      await expect(deafen).toHaveAttribute("data-deafened", "true");
      await expect(mute).toHaveAttribute("data-mic-state", "deafened");
      await release(page, "toggle_voice_deafen");
      await expect(mute).toHaveAttribute("data-mic-state", "deafened");

      await deafen.click();
      await expect(mute).toHaveAttribute("data-mic-state", "live");
      await expect(deafen).toHaveAttribute("data-deafened", "false");
    });

    test("the tray mounts with the join and Leave aborts a join that hangs", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);

      await hold(page, "join_voice_channel");
      await page.getByTestId("voice-join-cta").click();
      // Same frame as the click: tray present, controls parked, Leave live, local tile connecting.
      const foot = page.locator(".vs-foot");
      await expect(foot).toHaveAttribute("data-phase", "joining");
      await expect(page.getByTestId("voice-tray-mute")).toBeDisabled();
      await expect(page.getByTestId("voice-tray-leave")).toBeEnabled();
      await expect(page.getByTestId(`voice-tile-connecting-${LOCAL_IDENTITY}`)).toBeVisible();

      await page.getByTestId("voice-tray-leave").click();
      await expect(page.getByTestId("voice-join-cta")).toBeVisible();
      await expect(page.getByTestId("status-bar-voice-error")).toHaveCount(0);

      // The parked join returns later: Rust is torn down and the UI stays idle.
      await release(page, "join_voice_channel");
      await expect.poll(() => invokeCount(page, "leave_voice_channel")).toBeGreaterThan(0);
      await expect(page.getByTestId("voice-join-cta")).toBeVisible();
      await expect(page.getByTestId("status-bar-voice-error")).toHaveCount(0);
    });

    test("a reconnect is shown in place; a lost connection says so instead of vanishing", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      await inject(page, { type: "participant_joined", identity: BOB_IDENTITY, name: "bob", is_muted: false });
      const bob = page.getByTestId(`voice-tile-${BOB_IDENTITY}`);
      await expect(bob).toBeVisible();

      await inject(page, { type: "reconnecting", attempt: 2 });
      await expect(page.getByTestId("voice-stage-reconnecting")).toContainText("attempt 2");
      await expect(page.getByTestId(`voice-tile-reconnecting-${LOCAL_IDENTITY}`)).toBeVisible();
      await expect(bob).toHaveClass(/stale/);
      await expect(page.getByTestId(strip.reconnecting)).toBeVisible();
      // Still joined: the tray never went away.
      await expect(page.getByTestId("voice-tray-mute")).toBeVisible();
      await expect(page.getByTestId("voice-join-cta")).toHaveCount(0);

      await inject(page, { type: "reconnected" });
      await expect(page.getByTestId("voice-stage-reconnecting")).toHaveCount(0);
      await expect(bob).not.toHaveClass(/stale/);
      await expect(page.getByTestId(strip.reconnecting)).toHaveCount(0);

      await inject(page, { type: "disconnected" });
      await expect(page.getByTestId("voice-join-cta")).toBeVisible();
      await expect(page.getByTestId("status-bar-voice-error")).toContainText("Voice connection lost");
      // Dismissing it stays dismissed (it must not resurface on the next state change).
      await page.getByTestId("status-bar-voice-error-dismiss").click();
      await expect(page.getByTestId("status-bar-voice-error")).toHaveCount(0);
      await inject(page, { type: "participant_left", identity: BOB_IDENTITY });
      await expect(page.getByTestId("status-bar-voice-error")).toHaveCount(0);
    });

    test("leaving on purpose never reports a lost connection", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      await page.getByTestId("voice-tray-leave").click();
      await expect(page.getByTestId("voice-join-cta")).toBeVisible();
      await inject(page, { type: "disconnected" });
      await expect(page.getByTestId("status-bar-voice-error")).toHaveCount(0);
    });

    test("connection quality starts unmeasured and follows the event", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      const tileQuality = page.getByTestId(`voice-tile-quality-${LOCAL_IDENTITY}`);
      await expect(tileQuality).toHaveAttribute("data-quality", "unknown");
      await expect(page.getByTestId(strip.quality)).toHaveAttribute("data-quality", "unknown");

      await inject(page, { type: "connection_quality_changed", identity: LOCAL_IDENTITY, quality: "good" });
      await expect(tileQuality).toHaveAttribute("data-quality", "good");
      await expect(page.getByTestId(strip.quality)).toHaveAttribute("data-quality", "good");
    });

    test("camera and screenshare pills draw busy while a track is coming up", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      await page.evaluate(() => (window as unknown as TestWindow).__pollisStore.shareStartStarting(false));
      await expect(page.getByTestId("voice-tray-screenshare")).toHaveAttribute("data-busy", "true");
      await page.evaluate(() => (window as unknown as TestWindow).__pollisStore.shareStopped());
      await expect(page.getByTestId("voice-tray-screenshare")).not.toHaveAttribute("data-busy", "true");

      await page.evaluate(() => (window as unknown as TestWindow).__pollisStore.cameraStartStarting());
      await expect(page.getByTestId("voice-tray-camera")).toHaveAttribute("data-busy", "true");
      await page.evaluate(() => (window as unknown as TestWindow).__pollisStore.cameraStopped());
      await expect(page.getByTestId("voice-tray-camera")).not.toHaveAttribute("data-busy", "true");
    });

    test("a camera failure is shown in the status bar and dismissing clears it", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      await page.evaluate(() => (window as unknown as TestWindow).__pollisStore.cameraFailed("The camera is in use by another app."));
      await expect(page.getByTestId("status-bar-camera-error")).toContainText("in use by another app");
      await page.getByTestId("status-bar-camera-error-dismiss").click();
      await expect(page.getByTestId("status-bar-camera-error")).toHaveCount(0);
    });
  });
}
