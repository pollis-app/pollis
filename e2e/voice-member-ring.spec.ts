// The speaking ring around a sidebar voice member's avatar hugs the avatar (#1789), in BOTH skins:
// measured on the rendered pixels, not on class names. RING_SHOTS=<dir> also writes the review crops.

import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_voice";
const VOICE_CHANNEL_ID = "c_lounge";
const VOICE_CHANNEL_NAME = "lounge";
const BOB_IDENTITY = "voice-u_bob";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

// 2x, so a half-pixel offset shows up as a whole one.
const DPR = 2;
test.use({ deviceScaleFactor: DPR });

type TestWindow = Window & { __pollisVoice: { injectEvent: (event: unknown) => void } };

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((data) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = data;
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    groups: [
      { id: GROUP_ID, name: "voice", owner_id: ME.id, created_at: "2026-01-01T00:00:00Z", current_user_role: "admin" },
    ],
    channels: {
      [GROUP_ID]: [{ id: VOICE_CHANNEL_ID, group_id: GROUP_ID, name: VOICE_CHANNEL_NAME, channel_type: "voice" }],
    },
    groupMembers: {
      [GROUP_ID]: [{ user_id: ME.id, username: "mia", role: "admin", joined_at: "2026-01-01T00:00:00Z" }],
    },
    messages: {},
    dmChannels: [],
    preferences: { skin },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar").getByRole("button", { name: VOICE_CHANNEL_NAME }).click();
  await page.getByTestId("voice-join-cta").click();
  await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
  await page.evaluate(
    (e) => (window as unknown as TestWindow).__pollisVoice.injectEvent(e),
    { type: "participant_joined", identity: BOB_IDENTITY, name: "bob", is_muted: false },
  );
  await page.evaluate(
    (e) => (window as unknown as TestWindow).__pollisVoice.injectEvent(e),
    { type: "speaking_started", identity: BOB_IDENTITY },
  );
}

interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** Bounding box, in CSS px relative to `clip`, of the screenshot's pixels within 48 of `rgb`. */
async function pixelBox(page: Page, png: Buffer, rgb: [number, number, number]): Promise<Box | null> {
  return page.evaluate(
    async ({ b64, rgb, dpr }) => {
      const img = new Image();
      img.src = `data:image/png;base64,${b64}`;
      await img.decode();
      const canvas = document.createElement("canvas");
      canvas.width = img.naturalWidth;
      canvas.height = img.naturalHeight;
      const ctx = canvas.getContext("2d")!;
      ctx.drawImage(img, 0, 0);
      const { data, width, height } = ctx.getImageData(0, 0, canvas.width, canvas.height);
      let left = Infinity;
      let top = Infinity;
      let right = -Infinity;
      let bottom = -Infinity;
      for (let y = 0; y < height; y++) {
        for (let x = 0; x < width; x++) {
          const i = (y * width + x) * 4;
          const d = Math.abs(data[i] - rgb[0]) + Math.abs(data[i + 1] - rgb[1]) + Math.abs(data[i + 2] - rgb[2]);
          if (d <= 48) {
            left = Math.min(left, x);
            top = Math.min(top, y);
            right = Math.max(right, x + 1);
            bottom = Math.max(bottom, y + 1);
          }
        }
      }
      if (left === Infinity) {
        return null;
      }
      return { left: left / dpr, top: top / dpr, right: right / dpr, bottom: bottom / dpr };
    },
    { b64: png.toString("base64"), rgb, dpr: DPR },
  );
}

for (const skin of SKINS) {
  test(`the speaking ring is concentric with the avatar and as tall as it is wide — ${skin}`, async ({ page }) => {
    await boot(page, skin);
    const row = page.getByTestId(`sidebar-voice-member-${VOICE_CHANNEL_ID}-u_bob`);
    await expect(row).toHaveAttribute("data-speaking", "true");
    const ringed = row.locator(":scope > span").first();

    const geometry = await ringed.evaluate((el) => {
      const avatar = el.firstElementChild as HTMLElement;
      const a = avatar.getBoundingClientRect();
      const shadow = getComputedStyle(el).boxShadow;
      // The ring is the shadow with a spread (Tailwind lays a zero-spread offset shadow first).
      const m = [...shadow.matchAll(/rgba?\((\d+),\s*(\d+),\s*(\d+)[^)]*\)\s+\S+\s+\S+\s+\S+\s+([\d.]+)px/g)]
        .find((s) => Number(s[4]) > 0);
      return {
        avatar: { left: a.left, top: a.top, right: a.right, bottom: a.bottom },
        rgb: m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null,
      };
    });
    expect(geometry.rgb, "the speaking member has no ring").not.toBeNull();

    // Crop well past the ring, so a ring larger than the avatar is caught whole; on the
    // trailing side stop short of the name, which a speaking row also draws in the accent.
    const pad = 8;
    const clip = {
      x: geometry.avatar.left - pad,
      y: geometry.avatar.top - pad,
      width: geometry.avatar.right - geometry.avatar.left + pad + 4,
      height: geometry.avatar.bottom - geometry.avatar.top + 2 * pad,
    };
    const png = await page.screenshot({ clip });
    if (process.env.RING_SHOTS) {
      await page.screenshot({
        path: path.join(process.env.RING_SHOTS, `ring-${skin}.png`),
        clip: { x: clip.x - 40, y: clip.y - 30, width: 200, height: 70 },
      });
    }
    const ring = await pixelBox(page, png, geometry.rgb as [number, number, number]);
    expect(ring, "no ring pixels on screen").not.toBeNull();

    // Ring and avatar in the same frame: CSS px from the crop's corner.
    const avatar = {
      left: pad,
      top: pad,
      right: pad + geometry.avatar.right - geometry.avatar.left,
      bottom: pad + geometry.avatar.bottom - geometry.avatar.top,
    };
    const inset = {
      left: avatar.left - ring!.left,
      top: avatar.top - ring!.top,
      right: ring!.right - avatar.right,
      bottom: ring!.bottom - avatar.bottom,
    };
    console.log(`[ring] ${skin}: avatar ${JSON.stringify(avatar)} ring ${JSON.stringify(ring)} outset ${JSON.stringify(inset)}`);
    // A 2px ring on every side, to within a device pixel.
    for (const side of ["left", "top", "right", "bottom"] as const) {
      expect(inset[side], `ring outset on the ${side}`).toBeGreaterThanOrEqual(2 - 1 / DPR);
      expect(inset[side], `ring outset on the ${side}`).toBeLessThanOrEqual(2 + 1 / DPR);
    }
    expect(Math.abs(inset.top - inset.bottom)).toBeLessThanOrEqual(1 / DPR);
    expect(Math.abs(inset.left - inset.right)).toBeLessThanOrEqual(1 / DPR);
  });
}
