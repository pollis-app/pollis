// Desktop primitives (#1964), both skins: one 2px focus ring offset by the ground, Button sizes on
// the type scale, `Select` in the skin's UI face with its label, `SectionHeader` in sentence case
// in Modern, and every weight in use a loaded face. Screenshots of Preferences, Voice & video and
// the Members panel land in `e2e/artifacts/primitives/` for review.

import { test, expect, type Page } from "./lib/zero-loss";

const ME = { id: "u-alice", email: "alice@example.com", username: "alice" };
const DM_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DMX";
const DM_MSG = "01HQ7Z3K9M2P5R8T1V4W6Y0DM1";

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const SHOTS = "artifacts/primitives";

function member(userId: string, username: string) {
  return { user_id: userId, username, added_by: ME.id, added_at: "" };
}

async function boot(page: Page, skin: Skin) {
  await page.addInitScript((preload) => {
    (window as unknown as Record<string, unknown>).__POLLIS_PRELOAD__ = preload;
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
  }, {
    session: ME,
    profile: { id: ME.id, username: ME.username },
    preferences: JSON.stringify({ skin, right_panel_open_by_default: true }),
    groups: [],
    channels: {},
    dmChannels: [
      {
        id: DM_ID,
        created_by: ME.id,
        created_at: new Date().toISOString(),
        members: [member(ME.id, "alice"), member("u-bob", "bob")],
      },
    ],
    messages: {
      [DM_ID]: [
        { id: DM_MSG, conversation_id: DM_ID, sender_id: "u-bob", content: "hi", sent_at: "2026-08-01T10:00:00.000Z" },
      ],
    },
  });
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

async function openPage(page: Page, name: string, pageTestId: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(name);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(pageTestId)).toBeVisible();
}

async function openDm(page: Page) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(`pollis://m/${DM_ID}/${DM_MSG}`);
  await expect(page.getByTestId(`message-${DM_MSG}`)).toBeVisible();
  await expect(page.getByTestId("right-panel")).toBeVisible();
}

/** The resolved first family of an element's computed `font-family`. */
async function firstFamily(page: Page, selector: string): Promise<string> {
  return page.locator(selector).first().evaluate((el) =>
    getComputedStyle(el).fontFamily.split(",")[0].replace(/["']/g, "").trim(),
  );
}

for (const skin of SKINS) {
  test.describe(`desktop primitives — ${skin}`, () => {
    test("no control draws a black focus band, and the ring is 2px offset by the ground", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "appearance", "preferences-page");
      await expect(page.locator(".focus\\:ring-offset-black, .ring-offset-black")).toHaveCount(0);

      // Keyboard focus onto the first Switch: the ring is the accent, 2px, offset by `bg`.
      const ring = await page.evaluate(() => {
        const sw = document.querySelector<HTMLElement>('[data-testid="preferences-page"] [role="switch"]');
        if (!sw) {
          return null;
        }
        return sw.className;
      });
      expect(ring).toContain("focus-visible:ring-2");
      expect(ring).toContain("focus-visible:ring-offset-bg");
      expect(ring).not.toMatch(/ring-4|ring-offset-black/);
      await page.screenshot({ path: `${SHOTS}/${skin}-preferences.png`, fullPage: true });
    });

    test("Voice & video picks devices with Select: the skin's UI face, its own label", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "Voice & video", "voice-settings-page");
      const selects = page.getByTestId("voice-settings-page").locator("select");
      await expect(selects.first()).toBeVisible();
      // Every select is named by a <label>, so it has an accessible name.
      const unnamed = await selects.evaluateAll((els) =>
        els.filter((el) => (el as HTMLSelectElement).labels?.length === 0).length,
      );
      expect(unnamed).toBe(0);
      const family = await firstFamily(page, '[data-testid="voice-settings-page"] select');
      expect(family).toBe(skin === "modern" ? "Geist Sans" : "DM Mono");
      // The field shares the button's corner (the per-skin control radius).
      const radii = await page.evaluate(() => {
        const sel = document.querySelector('[data-testid="voice-settings-page"] select');
        const root = getComputedStyle(document.documentElement).getPropertyValue("--radius-control").trim();
        return { select: sel ? getComputedStyle(sel).borderTopLeftRadius : "", root };
      });
      expect(radii.select).toBe(radii.root);
      await page.screenshot({ path: `${SHOTS}/${skin}-voice-video.png`, fullPage: true });
    });

    test("the Members panel's section headers come from SectionHeader", async ({ page }) => {
      await boot(page, skin);
      await openDm(page);
      const header = page.getByTestId("right-panel").locator("h2").first();
      await expect(header).toBeVisible();
      const transform = await header.evaluate((el) => getComputedStyle(el).textTransform);
      expect(transform).toBe(skin === "terminal" ? "uppercase" : "none");
      await page.getByTestId("right-panel").screenshot({ path: `${SHOTS}/${skin}-members.png` });
    });

    test("Button text is on the type scale and follows the font-size preference", async ({ page }) => {
      await boot(page, skin);
      await openPage(page, "appearance", "preferences-page");
      // The `Button` primitive's own class set; hand-rolled buttons are #1967's.
      const sizes = await page.evaluate(() => {
        const read = () =>
          Array.from(document.querySelectorAll<HTMLButtonElement>("button.inline-flex.gap-2.font-mono.font-medium.rounded-control"))
            .map((b) => parseFloat(getComputedStyle(b).fontSize));
        const labels = Array.from(document.querySelectorAll<HTMLButtonElement>("button.inline-flex.gap-2.font-mono.font-medium.rounded-control")).map((b) => `${b.dataset.testid ?? ""} ${b.textContent?.trim().slice(0, 30)} :: ${b.className.slice(0, 160)}`);
        const before = read();
        document.documentElement.style.fontSize = "20px";
        const after = read();
        document.documentElement.style.fontSize = "";
        return { before, after, labels };
      });
      expect(sizes.before.length).toBeGreaterThan(0);
      // rem sizes: every button grows with the root, none is pinned in px.
      sizes.before.forEach((b, i) => expect(sizes.after[i], sizes.labels[i]).toBeGreaterThan(b));
    });

    test("every weight in use is a loaded face, not a synthesized one", async ({ page }) => {
      await boot(page, skin);
      const loaded = await page.evaluate(async () => {
        const faces = ["Geist Sans", "Atkinson Hyperlegible Next"];
        const out: Record<string, boolean> = {};
        for (const face of faces) {
          for (const weight of [400, 500, 600, 700]) {
            const spec = `${weight} 16px "${face}"`;
            await document.fonts.load(spec);
            out[spec] = document.fonts.check(spec);
          }
        }
        out['500 16px "DM Mono"'] = (await document.fonts.load('500 16px "DM Mono"')).length > 0;
        return out;
      });
      for (const [spec, ok] of Object.entries(loaded)) {
        expect(ok, spec).toBe(true);
      }
      const faces = await page.evaluate(() =>
        Array.from(document.fonts).filter((f) => f.status === "loaded").map((f) => `${f.family} ${f.weight}`),
      );
      for (const w of ["500", "600", "700"]) {
        expect(faces.some((f) => f.includes("Atkinson") && f.endsWith(w)), `Atkinson ${w}`).toBe(true);
        expect(faces.some((f) => f.includes("Geist") && f.endsWith(w)), `Geist ${w}`).toBe(true);
      }
    });
  });
}
