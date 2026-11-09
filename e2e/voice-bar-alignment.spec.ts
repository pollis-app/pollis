// The in-channel voice controls bar and the right panel's "Details" handle row share one height (#1790),
// so their top borders are one line, in BOTH skins and at several font sizes. Measured on rendered pixels.
// VOICE_BAR_SHOTS=<dir> also writes the review crops.

import path from "node:path";
import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u_me", email: "me@pollis.test", username: "mia" };
const GROUP_ID = "g_voice";
const VOICE_CHANNEL_ID = "c_lounge";
const VOICE_CHANNEL_NAME = "lounge";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];
const FONT_SIZES = ["13", "15", "18"] as const;

const DPR = 2;
test.use({ deviceScaleFactor: DPR, viewport: { width: 1280, height: 720 } });

async function boot(page: Page, skin: Skin, fontSize: string) {
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
    preferences: { skin, font_size: fontSize, right_panel_open_by_default: true },
  });
  await page.goto("/");
  await expect(page.getByTestId("app-ready")).toBeAttached();
  await page.getByTestId("sidebar").getByRole("button", { name: VOICE_CHANNEL_NAME }).click();
  await page.getByTestId("voice-join-cta").click();
  await expect(page.getByTestId("voice-tray-mute")).toBeEnabled();
  await expect(page.getByTestId("right-panel-close")).toBeVisible();
}

/** CSS-px y of the first row in a 1px-wide strip at `x` where the colour steps (the top border), searching `from`..`to`. */
async function borderTop(page: Page, x: number, from: number, to: number): Promise<number | null> {
  const png = await page.screenshot({ clip: { x, y: from, width: 1, height: to - from } });
  const row = await page.evaluate(
    async ({ b64 }) => {
      const img = new Image();
      img.src = `data:image/png;base64,${b64}`;
      await img.decode();
      const canvas = document.createElement("canvas");
      canvas.width = img.naturalWidth;
      canvas.height = img.naturalHeight;
      const ctx = canvas.getContext("2d")!;
      ctx.drawImage(img, 0, 0);
      const { data, width, height } = ctx.getImageData(0, 0, canvas.width, canvas.height);
      // Middle column of the strip, away from any antialiased edge.
      const col = Math.floor(width / 2);
      const px = (y: number) => {
        const i = (y * width + col) * 4;
        return [data[i], data[i + 1], data[i + 2]];
      };
      for (let y = 1; y < height; y++) {
        const a = px(y - 1);
        const b = px(y);
        if (Math.abs(a[0] - b[0]) + Math.abs(a[1] - b[1]) + Math.abs(a[2] - b[2]) > 12) {
          return y;
        }
      }
      return null;
    },
    { b64: png.toString("base64") },
  );
  return row === null ? null : from + row / DPR;
}

for (const skin of SKINS) {
  for (const fontSize of FONT_SIZES) {
    test(`voice bar and Details row share one height and one top border — ${skin}, ${fontSize}px`, async ({ page }) => {
      await boot(page, skin, fontSize);
      const foot = page.locator(".vs-foot");
      const handle = page.getByTestId("right-panel-close");
      const f = (await foot.boundingBox())!;
      const h = (await handle.boundingBox())!;

      // Both rest on the same floor, so one height means one top border.
      expect(Math.abs(f.y + f.height - (h.y + h.height)), "the rows' bottoms differ").toBeLessThanOrEqual(0.5);

      // The drawn borders, found in the pixels: 4px in from each row's start, clear of any control.
      const footLine = await borderTop(page, f.x + 4, f.y - 6, f.y + 6);
      const handleLine = await borderTop(page, h.x + 4, h.y - 6, h.y + 6);
      console.log(
        `[voice-bar] ${skin} ${fontSize}px: voice bar ${f.height}px tall, border at y=${footLine}; ` +
          `Details row ${h.height}px tall, border at y=${handleLine}`,
      );
      if (process.env.VOICE_BAR_SHOTS) {
        const seam = Math.min(f.y, h.y);
        await page.screenshot({
          path: path.join(process.env.VOICE_BAR_SHOTS, `voice-bar-${skin}-${fontSize}.png`),
          clip: { x: h.x - 260, y: seam - 30, width: 420, height: 90 },
        });
      }
      expect(footLine).not.toBeNull();
      expect(handleLine).not.toBeNull();
      expect(f.height).toBe(h.height);
      expect(footLine).toBe(handleLine);
    });
  }
}
