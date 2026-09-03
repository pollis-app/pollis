/*
 * Screen-share picker — the "share sound" switch versus its stored default
 * (#1040).
 *
 * The picker seeds its switch from the `screen_share_audio` preference, which
 * arrives asynchronously over IPC. A user who flips the switch before that
 * reply lands used to have their choice overwritten by the reply a few
 * milliseconds later — a race that real IPC latency makes reachable and the
 * mock's microtask-fast replies make invisible. `window.__tauriHold` parks
 * the `get_preferences` reply so the spec can widen that window on purpose,
 * flip the switch inside it, and then let the reply through.
 *
 * The mock never publishes, so `start_screen_share` is asserted by the
 * arguments the app sent, read back from `window.__tauriLastArgs`.
 */

import { test, expect, type Page } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_voice";
const TEXT_CHANNEL_ID = "c_general";
const VOICE_CHANNEL_ID = "c_lounge";

const DISPLAY = { id: "d1", name: "Display 1", width: 1920, height: 1080 };

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
    preferences,
  };
}

type TestWindow = Window & {
  __pollisStore: Record<string, Function>;
  __tauriHold: (cmd: string) => void;
  __tauriRelease: (cmd: string) => void;
  __tauriLastArgs: Record<string, unknown>;
};

/** Boot into the voice channel, joined, with the share still idle. */
async function joinVoice(page: Page, preferences: Record<string, unknown>) {
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

/** Open the in-app picker the way the macOS/Windows enumerate path does,
 *  with the preference reply optionally parked. */
async function openPicker(page: Page, holdPreferences: boolean) {
  await page.evaluate(
    ({ hold, display }) => {
      const w = window as unknown as TestWindow;
      if (hold) {
        w.__tauriHold("get_preferences");
      }
      w.__pollisStore.shareStartPicking({ displays: [display], windows: [] });
    },
    { hold: holdPreferences, display: DISPLAY },
  );
  await expect(page.getByTestId("screen-share-picker")).toBeVisible();
}

/** Let the parked replies through and give React a frame to apply them. */
async function releasePreferences(page: Page) {
  await page.evaluate(() => {
    (window as unknown as TestWindow).__tauriRelease("get_preferences");
  });
  await page.evaluate(
    () =>
      new Promise<void>((done) =>
        requestAnimationFrame(() => requestAnimationFrame(() => done())),
      ),
  );
}

test.describe("screen-share picker — share sound switch", () => {
  test("a stored default seeds an untouched switch", async ({ page }) => {
    await joinVoice(page, { screen_share_audio: true });
    await openPicker(page, false);
    const toggle = page.getByTestId("screen-share-audio-toggle");
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(page.getByTestId("screen-share-audio-scope")).toBeVisible();
  });

  test("flipping the switch before the stored default lands keeps the user's choice", async ({
    page,
  }) => {
    // Stored default: off. The user turns it on while the reply is in flight.
    await joinVoice(page, { screen_share_audio: false });
    await openPicker(page, true);
    const toggle = page.getByTestId("screen-share-audio-toggle");
    await expect(toggle).toHaveAttribute("aria-checked", "false");

    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "true");

    await releasePreferences(page);
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(page.getByTestId("screen-share-audio-scope")).toBeVisible();

    // And the share the user then starts carries that choice, not the default.
    await page.getByTestId(`screen-share-source-display-${DISPLAY.id}`).click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as TestWindow).__tauriLastArgs["start_screen_share"],
        ),
      )
      .toMatchObject({ selection: { kind: "display", id: DISPLAY.id }, withAudio: true });
  });

  test("turning a stored default off before it lands also wins", async ({ page }) => {
    // The mirror image: stored on, user turns it off before the reply.
    await joinVoice(page, { screen_share_audio: true });
    await openPicker(page, true);
    const toggle = page.getByTestId("screen-share-audio-toggle");
    // Off until the preference arrives; the user flips it on and back off,
    // which is the only way to express "off" against an unseeded switch.
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    await toggle.click();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "false");

    await releasePreferences(page);
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    await expect(page.getByTestId("screen-share-audio-scope")).toBeHidden();
  });
});
