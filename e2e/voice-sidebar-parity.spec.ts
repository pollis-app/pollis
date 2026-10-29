// Persistent voice-strip controls (#849, #891): both skins show four distinct mic states and can undeafen.

import { test, expect, type Page } from "./lib/zero-loss";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const TEXT_CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const VOICE_CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0VCH";
const VOICE_CHANNEL_NAME = "standup";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

/** Where each skin keeps the always-present controls (terminal VoiceBar, refined sidebar panel). */
const STRIP = {
  terminal: {
    strip: "voice-bar",
    mic: "voice-bar-mute-button",
    deafen: "voice-bar-deafen-button",
  },
  refined: {
    strip: "sidebar-voice-strip",
    mic: "sidebar-voice-mute",
    deafen: "sidebar-voice-deafen",
  },
} as const;

type InputMode = "voice_activity" | "push_to_talk";

function preloadState(
  skin: Skin,
  inputMode: InputMode,
  nullGateCommands: string[] = [],
) {
  return {
    // Commands the mock should answer with nothing instead of a gate
    // snapshot. Empty for every test but the #888 regression below.
    nullGateCommands,
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin, voice_input_mode: inputMode }),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: USER.id,
        created_at: new Date().toISOString(),
      },
    ],
    channels: {
      [GROUP_ID]: [
        {
          id: TEXT_CHANNEL_ID,
          group_id: GROUP_ID,
          name: "general",
          channel_type: "text",
        },
        {
          id: VOICE_CHANNEL_ID,
          group_id: GROUP_ID,
          name: VOICE_CHANNEL_NAME,
          channel_type: "voice",
        },
      ],
    },
    messages: {},
  };
}

async function boot(
  page: Page,
  skin: Skin,
  inputMode: InputMode = "voice_activity",
  nullGateCommands: string[] = [],
) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
    // `@tauri-apps/api/window` is NOT vite-aliased (only `/core` and `/event`
    // are), so the real module runs and reads `__TAURI_INTERNALS__` directly.
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
  }, preloadState(skin, inputMode, nullGateCommands));
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

/** Join via the UI and wait for the strip, which mounts only once the join completes. */
async function joinVoice(page: Page, skin: Skin) {
  await page
    .getByTestId("sidebar")
    .getByRole("button", { name: VOICE_CHANNEL_NAME })
    .click();
  await page.getByTestId("voice-join-cta").click();
  await expect(page.getByTestId(STRIP[skin].strip)).toBeVisible();
}

/** How one control looks: lucide glyph plus resolved paint (skins carry emphasis differently). */
interface ControlAppearance {
  icon: string;
  color: string;
  background: string;
  borderStyle: string;
}

/** Mic and deafen together: muted and deafened differ only by the deafen control. */
interface StripAppearance {
  micState: string | null;
  deafened: string | null;
  mic: ControlAppearance;
  deafen: ControlAppearance;
}

/** Park the pointer away so readings are the resting paint, not the hover one. */
async function restPointer(page: Page) {
  await page.mouse.move(0, 0);
}

async function readStrip(page: Page, skin: Skin): Promise<StripAppearance> {
  const ids = STRIP[skin];
  return page.evaluate(
    ({ micId, deafenId }) => {
      const pick = (id: string): HTMLElement => {
        const el = document.querySelector<HTMLElement>(`[data-testid="${id}"]`);
        if (!el) {
          throw new Error(`missing control: ${id}`);
        }
        return el;
      };
      const appearanceOf = (el: HTMLElement) => {
        const style = getComputedStyle(el);
        return {
          icon: el.querySelector("svg")?.getAttribute("class") ?? "",
          color: style.color,
          background: style.backgroundColor,
          borderStyle: style.borderTopStyle,
        };
      };
      const mic = pick(micId);
      const deafen = pick(deafenId);
      return {
        micState: mic.getAttribute("data-mic-state"),
        deafened: deafen.getAttribute("data-deafened"),
        mic: appearanceOf(mic),
        deafen: appearanceOf(deafen),
      };
    },
    { micId: ids.mic, deafenId: ids.deafen },
  );
}

/** True when the mic glyph is the crossed-out one rather than the open one. */
function micIsCrossedOut(a: StripAppearance): boolean {
  return a.mic.icon.includes("mic-off");
}

for (const skin of SKINS) {
  const ids = STRIP[skin];

  test.describe(`persistent voice strip — ${skin} skin`, () => {
    // Crisper artifacts: these controls are ~1.5rem and the whole point of the
    // ticket is whether a human can tell them apart in a screenshot.
    test.use({ deviceScaleFactor: 3 });

    test("the strip carries BOTH a mic control and a deafen control", async ({
      page,
    }) => {
      await boot(page, skin);
      await joinVoice(page, skin);

      // #891: refined shipped the mic half only, so undeafening from the
      // always-present strip was impossible.
      await expect(page.getByTestId(ids.mic)).toBeVisible();
      await expect(page.getByTestId(ids.deafen)).toBeVisible();
    });

    test("voice-activity join reads as a live, open mic", async ({ page }) => {
      await boot(page, skin);
      await joinVoice(page, skin);

      const live = await readStrip(page, skin);
      expect(live.micState).toBe("live");
      expect(live.deafened).toBe("false");
      expect(micIsCrossedOut(live)).toBe(false);
    });

    test("push-to-talk idle is NOT drawn as a live mic", async ({ page }) => {
      await boot(page, skin, "push_to_talk");
      await joinVoice(page, skin);

      // The whole defect: idle push-to-talk looking like you are transmitting.
      const idle = await readStrip(page, skin);
      expect(idle.micState).toBe("ptt-idle");
      // Nor as a mute — the user has not muted themselves.
      expect(idle.deafened).toBe("false");
      expect(micIsCrossedOut(idle)).toBe(false);
      await expect(page.getByTestId(ids.mic)).toHaveAttribute(
        "aria-label",
        /push to talk armed/i,
      );
    });

    test("muting and deafening are told apart, and deafen is reversible", async ({
      page,
    }) => {
      await boot(page, skin);
      await joinVoice(page, skin);

      await page.getByTestId(ids.mic).click();
      await restPointer(page);
      const muted = await readStrip(page, skin);
      expect(muted.micState).toBe("muted");
      // A plain mute must not claim the ear is closed.
      expect(muted.deafened).toBe("false");
      expect(micIsCrossedOut(muted)).toBe(true);

      // Unmute, then deafen from a clean slate.
      await page.getByTestId(ids.mic).click();
      await expect(page.getByTestId(ids.mic)).toHaveAttribute("data-mic-state", "live");

      await page.getByTestId(ids.deafen).click();
      await restPointer(page);
      const deafened = await readStrip(page, skin);
      expect(deafened.micState).toBe("deafened");
      expect(deafened.deafened).toBe("true");
      expect(micIsCrossedOut(deafened)).toBe(true);
      // Deafened and muted share the crossed mic; the deafen half is what
      // separates them, so it has to actually change — glyph AND emphasis.
      expect(deafened.deafen.icon).not.toBe(muted.deafen.icon);
      expect(JSON.stringify(deafened.deafen)).not.toBe(JSON.stringify(muted.deafen));

      // The point of #891: the strip can undeafen. Deafen implies mute, so
      // undeafening restores the mute state it displaced — here, unmuted.
      await page.getByTestId(ids.deafen).click();
      await restPointer(page);
      const undeafened = await readStrip(page, skin);
      expect(undeafened.micState).toBe("live");
      expect(undeafened.deafened).toBe("false");
      expect(micIsCrossedOut(undeafened)).toBe(false);
    });

    test("undeafening restores a mute it displaced rather than opening the mic", async ({
      page,
    }) => {
      await boot(page, skin);
      await joinVoice(page, skin);

      await page.getByTestId(ids.mic).click();
      await expect(page.getByTestId(ids.mic)).toHaveAttribute("data-mic-state", "muted");
      await page.getByTestId(ids.deafen).click();
      await expect(page.getByTestId(ids.mic)).toHaveAttribute("data-mic-state", "deafened");

      await page.getByTestId(ids.deafen).click();
      // Back to muted, NOT live — undeafen must never surprise the room.
      await expect(page.getByTestId(ids.mic)).toHaveAttribute("data-mic-state", "muted");
      await expect(page.getByTestId(ids.deafen)).toHaveAttribute("data-deafened", "false");
    });

    test("all four mic states render differently from one another", async ({
      page,
    }) => {
      const seen: Record<string, StripAppearance> = {};

      // `ptt-idle` needs its own session: the input mode is a preference read
      // at boot, and a join deliberately keeps it while clearing mute/deafen.
      // Every reading is taken at rest — see `restPointer`.
      const capture = async (state: string) => {
        await restPointer(page);
        seen[state] = await readStrip(page, skin);
        await page.getByTestId(ids.strip).screenshot({
          path: `artifacts/voice-strip-${skin}-${state}.png`,
        });
      };

      await boot(page, skin, "push_to_talk");
      await joinVoice(page, skin);
      await capture("ptt-idle");

      await boot(page, skin);
      await joinVoice(page, skin);
      await capture("live");

      await page.getByTestId(ids.mic).click();
      await capture("muted");

      await page.getByTestId(ids.mic).click();
      await page.getByTestId(ids.deafen).click();
      await capture("deafened");

      // Each state reports itself…
      for (const [name, appearance] of Object.entries(seen)) {
        expect(appearance.micState).toBe(name);
      }

      // …and each one LOOKS different (full appearance of both halves).
      const signatures = Object.values(seen).map((a) => JSON.stringify(a));
      expect(new Set(signatures).size).toBe(4);

      // Spelled out for the two pairs that are easy to collapse by accident:
      // live vs armed-idle share the open mic, muted vs deafened the crossed one.
      expect(JSON.stringify(seen.live.mic)).not.toBe(
        JSON.stringify(seen["ptt-idle"].mic),
      );
      expect(seen.muted.deafen.icon).not.toBe(seen.deafened.deafen.icon);
    });

    test("hovering a muted mic does not repaint it as a live one", async ({
      page,
    }) => {
      await boot(page, skin);
      await joinVoice(page, skin);

      await page.getByTestId(ids.mic).hover();
      const liveHovered = await readStrip(page, skin);

      await page.getByTestId(ids.mic).click();
      await page.getByTestId(ids.mic).hover();
      const mutedHovered = await readStrip(page, skin);

      // Read while hovered: a generic hover repaint must not erase the mute distinction.
      expect(mutedHovered.micState).toBe("muted");
      expect(JSON.stringify(mutedHovered.mic)).not.toBe(
        JSON.stringify(liveHovered.mic),
      );
    });
  });
}

/** #888: a gate command returning nothing is reported, not swallowed as a TypeError (asserted on the console). */
test("a gate command returning nothing is reported, not swallowed as a TypeError", async ({
  page,
}) => {
  const messages: string[] = [];
  page.on("console", (msg) => {
    messages.push(`${msg.type()}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => {
    messages.push(`pageerror: ${err.message}`);
  });

  await boot(page, "terminal", "voice_activity", ["toggle_voice_mute"]);
  await joinVoice(page, "terminal");

  const before = await readStrip(page, "terminal");
  await page.getByTestId(STRIP.terminal.mic).click();
  await expect
    .poll(() => messages.some((m) => m.includes("toggle_voice_mute")))
    .toBe(true);

  // The failure is named, not inferred from a stack trace.
  expect(
    messages.some(
      (m) => m.includes("returned no gate state") && m.startsWith("error:"),
    ),
  ).toBe(true);
  // Not a deref blowing up in the handler (scoped to gate fields; ignore unregisterListener noise).
  expect(
    messages.filter(
      (m) => m.includes("transmitting") || m.includes("self_muted"),
    ),
  ).toEqual([]);

  // An unknown gate must leave the last good state alone rather than write a
  // half-built one.
  expect(await readStrip(page, "terminal")).toEqual(before);
});
