/*
 * Push-to-talk and deafen (#849) — the controls, and whether their states are
 * actually TELLABLE APART on screen. BOTH skins.
 *
 * The claim this guards is the whole point of the ticket: `ptt-idle` is not a
 * mute. The user has not muted themselves, they are simply not holding the key,
 * and drawing that as a mute is what makes push-to-talk feel broken. Deafened
 * is a third thing again. So every assertion here is a *difference* assertion —
 * `data-mic-state` for what the UI believes, and the computed colours/borders
 * for what a person would actually see.
 *
 * ## What is driven, and why
 *
 * The gate is authored in Rust (`pollis-core/src/commands/voice/gate.rs`) and
 * mirrored into the store; the renderer only ever displays it. Clicking mute in
 * this environment cannot round-trip, because `VoiceSessionManager.applyGate`
 * refuses unless its own LiveKit session is joined and there is no LiveKit here.
 * So these tests push the Rust-authored snapshot onto the store through
 * `window.__pollisStore` (exposed by `main.tsx` for exactly this) and assert
 * what gets drawn. That is the honest split: the transitions themselves have
 * unit tests in `gate.rs`; what had none was whether the four states look
 * different.
 *
 * The one place a real round trip IS possible is the settings toggle, because
 * `setInputMode` runs with `requireJoined: false` — so that test drives a click
 * and reads the mock's gate back.
 */

import { test, expect, type Page, type Locator } from "@playwright/test";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_voice";
const TEXT_CHANNEL_ID = "c_general";
const VOICE_CHANNEL_ID = "c_lounge";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

/** A snapshot in the shape `VoiceGateState` travels in. */
interface Gate {
  mode: "voice_activity" | "push_to_talk";
  self_muted: boolean;
  deafened: boolean;
  ptt_held: boolean;
  transmitting: boolean;
}

const LIVE: Gate = {
  mode: "voice_activity",
  self_muted: false,
  deafened: false,
  ptt_held: false,
  transmitting: true,
};
const MUTED: Gate = { ...LIVE, self_muted: true, transmitting: false };
// Push-to-talk, armed, key not held. The state this whole spec exists for.
const PTT_IDLE: Gate = {
  mode: "push_to_talk",
  self_muted: false,
  deafened: false,
  ptt_held: false,
  transmitting: false,
};
const PTT_HELD: Gate = { ...PTT_IDLE, ptt_held: true, transmitting: true };
// `deafened ⇒ self_muted` is an invariant of the Rust gate, so a snapshot
// that broke it could never arrive here.
const DEAFENED: Gate = {
  mode: "voice_activity",
  self_muted: true,
  deafened: true,
  ptt_held: false,
  transmitting: false,
};

function preloadFor(skin: Skin) {
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
    preferences: { skin },
  };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, preloadFor(skin));
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  // The controls use `transition-colors`, so the moment `data-mic-state`
  // flips the computed colour is still the PREVIOUS state's and only
  // animates to the new one. Sampling appearance right after the attribute
  // settles therefore read a stale colour perhaps a third of the time, and
  // two genuinely different states compared equal. These tests are about
  // which colour a state lands on, never how it gets there.
  await page.addStyleTag({
    content: "*, *::before, *::after { transition: none !important; animation: none !important; }",
  });
}

/** Walk into the voice channel and put the session in the joined state. */
async function joinVoiceChannel(page: Page, skin: Skin) {
  await boot(page, skin);

  await page.getByTestId("menu-item-groups").click();
  await page.getByTestId(`group-option-${GROUP_ID}`).click();
  await page.getByTestId(`channel-option-${VOICE_CHANNEL_ID}`).click();
  // Until the session joins, the stage shows its observer preview and a Join
  // CTA; the tray only mounts once `voiceState.kind === 'joined'`.
  await expect(page.getByTestId("voice-join-cta")).toBeVisible();

  // The store transition the real join event drives. `voiceJoined` only
  // accepts a `joining` predecessor, so both steps are needed.
  await page.evaluate((channelId) => {
    const store = (window as unknown as { __pollisStore: Record<string, Function> })
      .__pollisStore;
    store.voiceStartJoining(channelId, null);
    store.voiceJoined();
  }, VOICE_CHANNEL_ID);

  await expect(page.getByTestId("voice-tray-mute")).toBeVisible();
  await expect(page.getByTestId("voice-tray-deafen")).toBeVisible();
}

/** Push a Rust-authored gate snapshot onto the store. */
async function setGate(page: Page, gate: Gate) {
  await page.evaluate((next) => {
    (window as unknown as { __pollisStore: { voiceSetGate: (g: unknown) => void } })
      .__pollisStore.voiceSetGate(next);
  }, gate);
}

/** What a person actually sees: colour, fill, and border treatment. */
async function appearanceOf(locator: Locator): Promise<string> {
  return locator.evaluate((el) => {
    const style = getComputedStyle(el);
    return [
      style.color,
      style.backgroundColor,
      style.borderColor,
      style.borderStyle,
      el.innerHTML,
    ].join("|");
  });
}

for (const skin of SKINS) {
  test.describe(`push-to-talk + deafen — ${skin} skin`, () => {
    test("the four mic states are labelled distinctly", async ({ page }) => {
      await joinVoiceChannel(page, skin);
      const mute = page.getByTestId("voice-tray-mute");

      await setGate(page, LIVE);
      await expect(mute).toHaveAttribute("data-mic-state", "live");

      await setGate(page, MUTED);
      await expect(mute).toHaveAttribute("data-mic-state", "muted");

      await setGate(page, PTT_IDLE);
      await expect(mute).toHaveAttribute("data-mic-state", "ptt-idle");

      // Holding the key is transmitting, so it reads as live — push-to-talk
      // must not look permanently "special" while you are actually speaking.
      await setGate(page, PTT_HELD);
      await expect(mute).toHaveAttribute("data-mic-state", "live");

      await setGate(page, DEAFENED);
      await expect(mute).toHaveAttribute("data-mic-state", "deafened");
      await expect(page.getByTestId("voice-tray-deafen")).toHaveAttribute(
        "data-deafened",
        "true",
      );
    });

    test("push-to-talk idle does not look like muted", async ({ page }) => {
      await joinVoiceChannel(page, skin);
      const mute = page.getByTestId("voice-tray-mute");

      await setGate(page, LIVE);
      const live = await appearanceOf(mute);

      await setGate(page, MUTED);
      const muted = await appearanceOf(mute);

      await setGate(page, PTT_IDLE);
      const pttIdle = await appearanceOf(mute);
      await page.screenshot({ path: `artifacts/voice-ptt-idle-${skin}.png` });

      // Three states, three appearances. The failure this catches is the easy
      // one to ship: idle push-to-talk drawn with the same red mute treatment.
      expect(pttIdle).not.toBe(muted);
      expect(pttIdle).not.toBe(live);
      expect(muted).not.toBe(live);

      // And specifically: armed-and-idle keeps the live Mic glyph (the mic is
      // ready, nothing is wrong) while a real mute switches to MicOff.
      await setGate(page, PTT_IDLE);
      const idleIcon = await mute.locator("svg").getAttribute("class");
      await setGate(page, MUTED);
      const mutedIcon = await mute.locator("svg").getAttribute("class");
      expect(idleIcon).not.toBe(mutedIcon);

      // The tooltip has to say which of the two it is, since the button is an
      // icon and nothing else.
      await setGate(page, PTT_IDLE);
      await expect(mute).toHaveAttribute("title", /push.to.talk|hold/i);
    });

    test("deafened is distinguishable from muted and from live", async ({ page }) => {
      await joinVoiceChannel(page, skin);
      const mute = page.getByTestId("voice-tray-mute");
      const deafen = page.getByTestId("voice-tray-deafen");

      await setGate(page, LIVE);
      const deafenLive = await appearanceOf(deafen);
      await expect(deafen).toHaveAttribute("data-deafened", "false");

      // Plain mute must not disturb the deafen control at all.
      await setGate(page, MUTED);
      expect(await appearanceOf(deafen)).toBe(deafenLive);
      await expect(deafen).toHaveAttribute("data-deafened", "false");

      await setGate(page, DEAFENED);
      const deafenOn = await appearanceOf(deafen);
      expect(deafenOn).not.toBe(deafenLive);
      await page.screenshot({ path: `artifacts/voice-deafened-${skin}.png` });

      // The pair as a whole is what separates deafened from muted: the mic
      // button reports `deafened` rather than `muted`, and the deafen button
      // lights up. (The mic GLYPH is shared between the two — both are MicOff,
      // which is correct: in both cases nothing is going out.)
      await expect(mute).toHaveAttribute("data-mic-state", "deafened");
      await expect(deafen).toHaveAttribute("title", /undeafen|deafen/i);
    });

    test("both controls are present and reachable, mic or no mic", async ({ page }) => {
      await joinVoiceChannel(page, skin);

      await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
      await expect(page.getByTestId("voice-tray-deafen")).toBeEnabled();

      // Listen-only: no capture device. The mute toggle becomes a static
      // indicator, but deafen stays live — incoming audio is still worth
      // silencing, which is the reason it is not gated on a microphone.
      await page.evaluate(() => {
        (window as unknown as { __pollisStore: { voiceSetMicAvailable: (v: boolean) => void } })
          .__pollisStore.voiceSetMicAvailable(false);
      });
      await expect(page.getByTestId("voice-tray-listen-only")).toBeVisible();
      await expect(page.getByTestId("voice-tray-mute")).toHaveCount(0);
      await expect(page.getByTestId("voice-tray-deafen")).toBeEnabled();
    });

    test("the input-mode toggle switches modes and explains the new one", async ({
      page,
    }) => {
      await boot(page, skin);

      await page.keyboard.press("Control+KeyK");
      await expect(page.getByTestId("search-panel")).toBeVisible();
      await page.getByTestId("search-panel-input").fill("Voice & Video");
      await page.keyboard.press("Enter");

      const select = page.getByTestId("voice-input-mode");
      await expect(select).toBeVisible();
      // Both choices stay visible, so which one is active needs no click to
      // discover.
      await expect(page.getByTestId("voice-input-mode-voice_activity")).toBeVisible();
      await expect(page.getByTestId("voice-input-mode-push_to_talk")).toBeVisible();
      await expect(select).toContainText("microphone is open whenever you are not muted");

      await page.getByTestId("voice-input-mode-push_to_talk").click();

      // The explanation follows the mode, and names the key you have to hold.
      await expect(select).toContainText("until you hold");
      await expect(select).toContainText("loses focus");

      await page.screenshot({ path: `artifacts/voice-settings-${skin}.png` });

      // It reached both sinks: persisted as a preference, and pushed into the
      // gate so a mid-call change takes effect now rather than at next join.
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as unknown as { __tauriMock: { preferences: Record<string, unknown> } })
                .__tauriMock.preferences.voice_input_mode,
          ),
        )
        .toBe("push_to_talk");
      await expect
        .poll(() =>
          page.evaluate(
            () =>
              (window as unknown as { __tauriMock: { voiceGate: { mode: string } } })
                .__tauriMock.voiceGate.mode,
          ),
        )
        .toBe("push_to_talk");

      // And back, so the toggle is a toggle rather than a one-way door.
      await page.getByTestId("voice-input-mode-voice_activity").click();
      await expect(select).toContainText("microphone is open whenever you are not muted");
    });
  });
}

/*
 * The terminal skin carries a second, always-on copy of these controls in the
 * bottom `VoiceBar`; AppShell mounts it only for that skin. The refined skin's
 * equivalent strip lives in `SidebarProfilePanel` and is NOT covered here —
 * see the note at the end of this file.
 */
test.describe("push-to-talk + deafen — terminal VoiceBar", () => {
  test("the persistent bar carries the same four states and a deafen toggle", async ({
    page,
  }) => {
    await joinVoiceChannel(page, "terminal");

    const mute = page.getByTestId("voice-bar-mute-button");
    const deafen = page.getByTestId("voice-bar-deafen-button");
    await expect(mute).toBeVisible();
    await expect(deafen).toBeVisible();

    // Wait for the button to actually reflect each gate BEFORE sampling how it
    // looks. `setGate` resolves when the command returns, not when React has
    // re-rendered, so reading the appearance straight after it sampled the
    // previous frame perhaps a third of the time — and a stale sample makes
    // two genuinely different states compare equal.
    const appearanceInState = async (gate: Parameters<typeof setGate>[1], state: string) => {
      await setGate(page, gate);
      await expect(mute).toHaveAttribute("data-mic-state", state);
      return appearanceOf(mute);
    };

    const live = await appearanceInState(LIVE, "live");
    const muted = await appearanceInState(MUTED, "muted");
    const pttIdle = await appearanceInState(PTT_IDLE, "ptt-idle");

    expect(pttIdle).not.toBe(muted);
    expect(pttIdle).not.toBe(live);

    await setGate(page, DEAFENED);
    await expect(mute).toHaveAttribute("data-mic-state", "deafened");
    await expect(deafen).toHaveAttribute("data-deafened", "true");

    await page.screenshot({ path: "artifacts/voice-bar-terminal.png" });
  });
});

/*
 * The gap this file used to record — refined's sidebar strip having no deafen
 * control and a two-state mic — was #891, and is closed. The strip now carries
 * the same four-state mic and a deafen toggle in both skins; the tests live in
 * `voice-sidebar-parity.spec.ts`, which asserts the four states are distinct in
 * rendered appearance rather than by state name.
 */
