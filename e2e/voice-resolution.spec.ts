// In-call resolution (#1902): while the user sends a screen share or a camera, the stage's Resolution
// panel changes the published ceiling of the running stream (`set_screen_share_max_height` /
// `set_camera_max_height`, no restart), and it is the same preference as Voice & Video settings.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_voice";
const TEXT_CHANNEL_ID = "c_general";
const VOICE_CHANNEL_ID = "c_lounge";
const CAMERA = { id: "cam-1", name: "FaceTime HD Camera" };

function preload(preferences: Record<string, unknown>) {
  return {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      {
        id: GROUP_ID,
        name: "voice",
        owner_id: ME.id,
        created_at: "2026-01-01T00:00:00Z",
        current_user_role: "admin",
      },
    ],
    channels: {
      [GROUP_ID]: [
        { id: TEXT_CHANNEL_ID, group_id: GROUP_ID, name: "general", channel_type: "text" },
        { id: VOICE_CHANNEL_ID, group_id: GROUP_ID, name: "lounge", channel_type: "voice" },
      ],
    },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" },
      ],
    },
    messages: {},
    dmChannels: [],
    cameras: { cameras: [CAMERA] },
    preferences,
  };
}

type TestWindow = Window & {
  __pollisStore: Record<string, Function>;
  __tauriLastArgs: Record<string, unknown>;
  __tauriInvokeCounts: Record<string, number>;
};

async function joinVoice(page: Page, preferences: Record<string, unknown> = {}) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preload(preferences));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();

  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${VOICE_CHANNEL_ID}`).click();
  await expect(page.getByTestId("voice-join-cta")).toBeVisible();

  await page.evaluate((channelId) => {
    const store = (window as unknown as TestWindow).__pollisStore;
    store.voiceStartJoining(channelId, null);
    store.voiceJoined();
  }, VOICE_CHANNEL_ID);
  await expect(page.getByTestId("voice-tray-mute")).toBeVisible();
}

/** The share as the backend's `local_started` leaves it. */
async function shareLive(page: Page) {
  await page.evaluate(() => {
    const store = (window as unknown as TestWindow).__pollisStore;
    store.shareStartStarting(false);
    store.shareStarted("tauri-local", { width: 1920, height: 1080 });
  });
  await expect(page.getByTestId("voice-tray-screenshare")).toHaveAttribute("aria-pressed", "true");
}

async function cameraLive(page: Page) {
  await page.evaluate(() => {
    const store = (window as unknown as TestWindow).__pollisStore;
    store.cameraStartStarting();
    store.cameraStarted("camera", { width: 1280, height: 720 });
  });
  await expect(page.getByTestId("voice-tray-camera")).toHaveAttribute("aria-pressed", "true");
}

function lastArgs(page: Page, cmd: string) {
  return page.evaluate((c) => (window as unknown as TestWindow).__tauriLastArgs[c], cmd);
}

function invokeCount(page: Page, cmd: string) {
  return page.evaluate((c) => (window as unknown as TestWindow).__tauriInvokeCounts[c] ?? 0, cmd);
}

/** What the mock DS holds for the preferences, once the throttled save lands. */
function storedPreference(page: Page, key: string) {
  return page.evaluate(async (k) => {
    const w = window as unknown as TestWindow;
    const args = w.__tauriLastArgs["save_preferences"] as { preferencesJson?: string } | undefined;
    return args?.preferencesJson ? JSON.parse(args.preferencesJson)[k] : undefined;
  }, key);
}

test.describe("in-call resolution", () => {
  test("is offered only while the user sends something", async ({ page }) => {
    await joinVoice(page);
    await expect(page.getByTestId("voice-resolution-toggle")).toHaveCount(0);

    await shareLive(page);
    const toggle = page.getByTestId("voice-resolution-toggle");
    await expect(toggle).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-pressed", "false");
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByTestId("voice-resolution-panel")).toBeVisible();

    // Stopping the share takes the panel and its button away.
    await page.evaluate(() => (window as unknown as TestWindow).__pollisStore.shareStopped());
    await expect(page.getByTestId("voice-resolution-panel")).toHaveCount(0);
    await expect(page.getByTestId("voice-resolution-toggle")).toHaveCount(0);
  });

  test("a pick changes the running share's published ceiling and the stored preference", async ({
    page,
  }) => {
    await joinVoice(page, { screen_share_max_height: 1080 });
    await shareLive(page);
    await page.getByTestId("voice-resolution-toggle").click();

    const panel = page.getByTestId("voice-resolution-panel");
    // Only the stream being sent: no camera list while the camera is off.
    await expect(panel.getByTestId("camera-resolution")).toHaveCount(0);
    await expect(panel.getByTestId("screenshare-resolution-1080")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(await invokeCount(page, "set_screen_share_max_height")).toBe(0);

    await panel.getByTestId("screenshare-resolution-720").click();
    // Same frame: the row is chosen before any reply.
    await expect(panel.getByTestId("screenshare-resolution-720")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await expect.poll(() => lastArgs(page, "set_screen_share_max_height")).toEqual({ maxHeight: 720 });
    await expect.poll(() => storedPreference(page, "screen_share_max_height")).toBe(720);

    // "Source" lifts the ceiling: 0 on the wire.
    await panel.getByTestId("screenshare-resolution-0").click();
    await expect.poll(() => lastArgs(page, "set_screen_share_max_height")).toEqual({ maxHeight: 0 });
    expect(await invokeCount(page, "set_screen_share_max_height")).toBe(2);
    // The share itself was never restarted.
    expect(await invokeCount(page, "start_screen_share")).toBe(0);
    expect(await invokeCount(page, "stop_screen_share")).toBe(0);
  });

  test("a pick changes the running camera's published ceiling", async ({ page }) => {
    await joinVoice(page);
    await cameraLive(page);
    await page.getByTestId("voice-resolution-toggle").click();

    const panel = page.getByTestId("voice-resolution-panel");
    await expect(panel.getByTestId("screenshare-resolution")).toHaveCount(0);
    // The camera's default is its own size.
    await expect(panel.getByTestId("camera-resolution-0")).toHaveAttribute("aria-checked", "true");

    await panel.getByTestId("camera-resolution-360").click();
    await expect.poll(() => lastArgs(page, "set_camera_max_height")).toEqual({ maxHeight: 360 });
    await expect.poll(() => storedPreference(page, "camera_max_height")).toBe(360);
    expect(await invokeCount(page, "stop_camera")).toBe(0);

    // Sharing too: both lists, side by side.
    await shareLive(page);
    await expect(panel.getByTestId("screenshare-resolution")).toBeVisible();
    await expect(panel.getByTestId("camera-resolution")).toBeVisible();  });

  test("the settings page and the call panel are one preference", async ({ page }) => {
    await joinVoice(page);
    await shareLive(page);
    await cameraLive(page);
    await page.getByTestId("voice-resolution-toggle").click();
    await page.getByTestId("screenshare-resolution-1440").click();
    await page.getByTestId("camera-resolution-720").click();

    await page.getByTestId("voice-settings-link").click();
    const settings = page.getByTestId("voice-settings-page");
    await expect(settings).toBeVisible();
    await expect(settings.getByTestId("screenshare-resolution-1440")).toHaveAttribute(
      "aria-checked",
      "true",
    );

    // A change on the settings page also reaches the running share.
    await settings.getByTestId("screenshare-resolution-720").click();
    await expect.poll(() => lastArgs(page, "set_screen_share_max_height")).toEqual({ maxHeight: 720 });

    // The camera list sits in the camera section, which needs a detected camera.
    const camera = settings.getByTestId("camera-resolution-720");
    await expect(camera).toHaveAttribute("aria-checked", "true");
    await settings.getByTestId("camera-resolution-1080").click();
    await expect.poll(() => lastArgs(page, "set_camera_max_height")).toEqual({ maxHeight: 1080 });

    // Back to the call: the panel reads the same preference.
    await page.getByRole("button", { name: "Back", exact: true }).click();
    await page.getByTestId("voice-resolution-toggle").click();
    await expect(page.getByTestId("screenshare-resolution-720")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await expect(page.getByTestId("camera-resolution-1080")).toHaveAttribute("aria-checked", "true");
  });

  test("a camera started from the picker carries the stored ceiling", async ({ page }) => {
    await joinVoice(page, { camera_max_height: 720 });
    await page.evaluate((cam) => {
      (window as unknown as TestWindow).__pollisStore.cameraStartPicking([cam]);
    }, CAMERA);
    await page.getByTestId(`camera-picker-device-${CAMERA.id}`).click();
    await expect
      .poll(() => lastArgs(page, "start_camera"))
      .toEqual({ deviceId: CAMERA.id, maxHeight: 720 });
  });
});
