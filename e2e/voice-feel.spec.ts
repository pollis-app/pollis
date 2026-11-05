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

// ---- Discord parity: roster, sharing line, layout, focus mode, shortcuts ----

const SHARE_KEY = "track-bob-screen";

for (const skin of SKINS) {
  test.describe(`voice parity — ${skin} skin`, () => {
    test("the sidebar lists who is in the room, with speaking and muted glyphs", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      await inject(page, { type: "participant_joined", identity: BOB_IDENTITY, name: "bob", is_muted: false });
      await inject(page, { type: "participant_joined", identity: "voice-u_carol", name: "carol", is_muted: true });

      const members = page.getByTestId(`sidebar-voice-members-${VOICE_CHANNEL_ID}`);
      await expect(members).toBeVisible();
      await expect(members.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_bob`)).toContainText("bob");
      await expect(members.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_carol`)).toHaveAttribute("data-muted", "true");
      await expect(members.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-${ME.id}`)).toBeVisible();

      await inject(page, { type: "speaking_started", identity: BOB_IDENTITY });
      await expect(members.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_bob`)).toHaveAttribute("data-speaking", "true");
      await inject(page, { type: "speaking_stopped", identity: BOB_IDENTITY });
      await expect(members.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_bob`)).not.toHaveAttribute("data-speaking", "true");

      await inject(page, { type: "participant_left", identity: BOB_IDENTITY });
      await expect(members.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_bob`)).toHaveCount(0);
    });

    test("a remote screenshare: 'X is sharing' outside the stage, grid/spotlight toggle, and focus mode without an overlay", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      await inject(page, { type: "participant_joined", identity: BOB_IDENTITY, name: "bob", is_muted: false });
      await page.evaluate(
        ({ identity, key }) =>
          (window as unknown as { __pollisVoice: { setScreenShare: Function } }).__pollisVoice.setScreenShare(identity, {
            trackKey: key,
            width: 1280,
            height: 720,
          }),
        { identity: BOB_IDENTITY, key: SHARE_KEY },
      );

      // Spotlight by default: the share is the big tile, the others are film cells.
      const screenTile = page.getByTestId(`voice-tile-${BOB_IDENTITY}:screen`);
      await expect(screenTile).toBeVisible();
      await expect(page.locator(".vs-spotlight")).toBeVisible();
      if (skin === "terminal") {
        await expect(page.getByTestId("voice-bar-sharing")).toContainText("bob is sharing");
      }
      const roster = page.getByTestId(`sidebar-voice-members-${VOICE_CHANNEL_ID}`);
      await expect(roster.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_bob`).getByLabel("Sharing their screen")).toBeVisible();

      // Grid keeps everyone equal while the stream continues.
      const layout = page.getByTestId("voice-layout-toggle");
      await expect(layout).toHaveAttribute("data-layout", "spotlight");
      await layout.click();
      await expect(layout).toHaveAttribute("data-layout", "grid");
      await expect(layout).toHaveAttribute("aria-pressed", "true");
      await expect(page.locator(".vs-spotlight")).toHaveCount(0);
      await expect(page.getByTestId("voice-channel-view")).toBeVisible();
      await expect(screenTile).toBeVisible();

      // The stream tile is a button: Enter spotlights it.
      await expect(screenTile).toHaveAttribute("role", "button");
      await screenTile.focus();
      await page.keyboard.press("Enter");
      await layout.click();
      await expect(page.locator(".vs-spot-main").getByTestId(`voice-tile-${BOB_IDENTITY}:screen`)).toBeVisible();

      // Fullscreen is focus mode inside the stage body: no overlay, header gone, tray kept, one canvas.
      await screenTile.hover();
      await screenTile.getByLabel("Open fullscreen").click();
      const viewer = page.getByTestId("screenshare-viewer");
      await expect(viewer).toBeVisible();
      await expect(viewer).toContainText("watching bob");
      expect(await viewer.evaluate((el) => getComputedStyle(el).position)).not.toBe("absolute");
      expect(await viewer.evaluate((el) => getComputedStyle(el).position)).not.toBe("fixed");
      await expect(page.locator(".vs-body").getByTestId("screenshare-viewer")).toBeVisible();
      await expect(page.getByTestId(`remote-video-tile-${SHARE_KEY}`)).toHaveCount(1);
      await expect(page.getByTestId("voice-tray-leave")).toBeVisible();
      await expect(page.getByTestId("voice-layout-toggle")).toHaveCount(0);

      await page.keyboard.press("Escape");
      await expect(viewer).toHaveCount(0);
      await expect(page.locator(".vs-spotlight")).toBeVisible();

      // Stream ends: the sharing line and the LIVE glyph go away.
      await page.evaluate((key) => (window as unknown as { __pollisVoice: { clearScreenShare: Function } }).__pollisVoice.clearScreenShare(key), SHARE_KEY);
      await expect(screenTile).toHaveCount(0);
      if (skin === "terminal") {
        await expect(page.getByTestId("voice-bar-sharing")).toHaveCount(0);
      }
    });

    test("camera and screenshare have keyboard shortcuts that read the live state", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      const mod = process.platform === "darwin" ? "Meta" : "Control";
      // Outside a room the combos do nothing; inside, they hit the same toggles as the pills.
      await page.keyboard.press(`${mod}+Shift+KeyV`);
      await expect.poll(() => invokeCount(page, "list_video_devices")).toBeGreaterThan(0);
      await page.keyboard.press(`${mod}+Shift+KeyS`);
      await expect.poll(() => invokeCount(page, "enumerate_screen_sources")).toBeGreaterThan(0);
    });
  });
}

// ---- Cross-lane contracts (A1 typed disconnect + speaker availability; A2 local meter while muted) ----

for (const skin of SKINS) {
  test.describe(`voice contracts — ${skin} skin`, () => {
    test("a typed disconnect reason is surfaced; a speaker failure is a notice, not a drop", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);

      await inject(page, { type: "speaker_availability", identity: LOCAL_IDENTITY, available: false });
      await expect(page.getByTestId("voice-stage-speaker-unavailable")).toBeVisible();
      await expect(page.getByTestId("voice-tray-mute")).toBeVisible();
      await inject(page, { type: "speaker_availability", identity: LOCAL_IDENTITY, available: true });
      await expect(page.getByTestId("voice-stage-speaker-unavailable")).toHaveCount(0);

      await inject(page, { type: "disconnected", error: { kind: "livekit_unreachable", detail: "signal failure" } });
      await expect(page.getByTestId("voice-join-cta")).toBeVisible();
      // Either A1's friendly wording or the raw typed reason: never silent.
      await expect(page.getByTestId("status-bar-voice-error")).toContainText(/voice server|livekit_unreachable/);
    });

    test("talking into a muted mic shows the muted hint on the local tile", async ({ page }) => {
      await boot(page, skin);
      await openVoiceChannel(page);
      await joinVoice(page);
      const hint = page.getByTestId(`voice-tile-muted-hint-${LOCAL_IDENTITY}`);
      await expect(hint).toHaveCount(0);

      await page.getByTestId("voice-tray-mute").click();
      await expect(page.getByTestId("voice-tray-mute")).toHaveAttribute("data-mic-state", "muted");
      await expect(hint).toBeAttached();
      await expect(hint).toBeHidden();

      // The local meter keeps running while muted; a loud frame lights the hint.
      await page.evaluate((id) => (window as unknown as { __pollisAudioLevels: { push: Function } }).__pollisAudioLevels.push(id, [0.6, 0.7, 0.5]), LOCAL_IDENTITY);
      await expect(hint).toBeVisible();
      await expect(page.getByTestId(`voice-tile-${LOCAL_IDENTITY}`)).toHaveAttribute("data-talking-muted", "true");

      // Unmuting drops it on the spot.
      await page.getByTestId("voice-tray-mute").click();
      await expect(hint).toHaveCount(0);
    });
  });
}
