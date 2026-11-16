// Locale smoke (#1725): every SHIPPED locale × both skins, through the key screens (sign-in, chat
// list, a conversation, the composer, settings, the Language page, PIN). Fails on a raw catalogue key on screen, the
// locale not taking, or layout overflow; saves a screenshot per screen. The locale list is the
// catalogue directory listing, so a new locale is covered by adding its catalogue — nothing here.
//
// Screenshots: artifacts/locale-smoke/<locale>-<skin>-<screen>.png. LOCALE_SMOKE=ja,ko narrows the run.

import fs from "node:fs";
import path from "node:path";

import { test, expect, type Page } from "./lib/zero-loss";

const LOCALES_DIR = path.join(__dirname, "..", "frontend", "src", "i18n", "locales");
const SHOTS_DIR = path.join(__dirname, "artifacts", "locale-smoke");

const ALL_LOCALES = fs
  .readdirSync(LOCALES_DIR)
  .filter((entry) => fs.statSync(path.join(LOCALES_DIR, entry)).isDirectory())
  .sort();
const ONLY = (process.env.LOCALE_SMOKE ?? "").split(",").map((s) => s.trim()).filter(Boolean);
const LOCALES = ONLY.length ? ALL_LOCALES.filter((l) => ONLY.includes(l)) : ALL_LOCALES;

const SKINS = ["terminal", "modern"] as const;
type Skin = (typeof SKINS)[number];

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };
const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";
const MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0LAT";

function flatten(object: Record<string, unknown>, prefix = "", out: string[] = []): string[] {
  for (const [key, value] of Object.entries(object)) {
    const full = prefix ? `${prefix}.${key}` : key;
    if (value && typeof value === "object") {
      flatten(value as Record<string, unknown>, full, out);
    } else {
      out.push(full);
    }
  }
  return out;
}

/** Every key i18next could print raw: `section.item`, `ns:section.item`, and plural families without the suffix. */
const RAW_KEYS: string[] = (() => {
  const out = new Set<string>();
  const dir = path.join(LOCALES_DIR, "en");
  for (const file of fs.readdirSync(dir).filter((f) => f.endsWith(".json"))) {
    const ns = file.replace(/\.json$/, "");
    for (const key of flatten(JSON.parse(fs.readFileSync(path.join(dir, file), "utf8")))) {
      const family = key.replace(/_(zero|one|two|few|many|other)$/, "");
      // A single-word key ("title") is also an English word; only dotted ones are unambiguous.
      for (const k of new Set([key, family])) {
        if (k.includes(".")) {
          out.add(k);
        }
        out.add(`${ns}:${k}`);
      }
    }
  }
  return [...out];
})();

function preloadState(skin: Skin, signedIn: boolean) {
  if (!signedIn) {
    return { preferences: JSON.stringify({ skin }) };
  }
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [{ id: GROUP_ID, name: "Acme", owner_id: USER.id, created_at: "2026-08-01T09:00:00.000Z" }],
    channels: { [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }] },
    groupMembers: {
      [GROUP_ID]: [
        { user_id: USER.id, username: "alice", role: "admin", joined_at: "2026-08-01T09:00:00.000Z" },
        { user_id: "u-bob", username: "bob", role: "member", joined_at: "2026-08-01T09:00:00.000Z" },
      ],
    },
    messages: {
      [CHANNEL_ID]: [
        {
          id: MESSAGE_ID,
          conversation_id: CHANNEL_ID,
          sender_id: "u-bob",
          content: "shipping notes for the london office",
          sent_at: "2026-08-01T10:00:00.000Z",
        },
      ],
    },
  };
}

/** Boot with `language` as the browser locale — the first-run auto-detect path, as on a fresh install. */
async function boot(page: Page, skin: Skin, language: string, signedIn: boolean) {
  await page.addInitScript(
    ({ preload, lang }) => {
      const w = window as unknown as Record<string, unknown>;
      w.__POLLIS_PRELOAD__ = preload;
      Object.defineProperty(navigator, "languages", { get: () => [lang], configurable: true });
      Object.defineProperty(navigator, "language", { get: () => lang, configurable: true });
      w.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
        plugins: {},
        transformCallback: () => 0,
        convertFileSrc: (p: string) => p,
        registerListener: () => {},
        unregisterListener: () => {},
        runCallback: () => {},
        invoke: () => Promise.resolve(null),
      };
    },
    { preload: preloadState(skin, signedIn), lang: language },
  );
  await page.goto("/");
  // The locale took: auto-detect resolved the OS tag to this catalogue.
  await expect(page.locator("html")).toHaveAttribute("lang", language);
}

/** Raw keys and overflow on the current screen, then a screenshot. */
async function audit(page: Page, language: string, skin: Skin, screen: string) {
  // Let fonts settle so a fallback swap does not move the measurements.
  await page.evaluate(() => document.fonts.ready);
  const found = await page.evaluate((keys) => {
    const raw = new Set(keys);
    const rawSeen: string[] = [];
    const visible = (el: Element) => {
      const r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== "hidden";
    };
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = node.textContent?.trim() ?? "";
      if (text && raw.has(text) && node.parentElement && visible(node.parentElement)) {
        rawSeen.push(text);
      }
    }
    for (const el of document.querySelectorAll("[aria-label], [placeholder], [title]")) {
      for (const attr of ["aria-label", "placeholder", "title"]) {
        const value = el.getAttribute(attr)?.trim();
        if (value && raw.has(value)) {
          rawSeen.push(`${attr}=${value}`);
        }
      }
    }
    // Overflow: the page scrolls sideways, or a control's label spills out of it unclipped.
    const overflow: string[] = [];
    const root = document.documentElement;
    if (root.scrollWidth > root.clientWidth + 1) {
      overflow.push(`page is ${root.scrollWidth - root.clientWidth}px wider than the window`);
    }
    for (const el of document.querySelectorAll("button, [role=tab], [role=radio], label")) {
      if (!visible(el) || !(el.textContent ?? "").trim()) {
        continue;
      }
      const cs = getComputedStyle(el);
      const clipped = cs.textOverflow === "ellipsis" || cs.overflowX === "auto" || cs.overflowX === "scroll";
      if (!clipped && el.scrollWidth > el.clientWidth + 2 && el.clientWidth > 0) {
        overflow.push(`<${el.tagName.toLowerCase()}> "${(el.textContent ?? "").trim().slice(0, 40)}" overflows by ${el.scrollWidth - el.clientWidth}px`);
      }
    }
    return { rawSeen, overflow };
  }, RAW_KEYS);
  fs.mkdirSync(SHOTS_DIR, { recursive: true });
  await page.screenshot({ path: path.join(SHOTS_DIR, `${language}-${skin}-${screen}.png`) });
  expect(found.rawSeen, `${language}/${skin}/${screen}: raw catalogue keys on screen`).toEqual([]);
  expect(found.overflow, `${language}/${skin}/${screen}: layout overflow`).toEqual([]);
}

async function searchAndOpen(page: Page, query: string) {
  await page.keyboard.press(process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK");
  await expect(page.getByTestId("search-panel")).toBeVisible();
  await page.getByTestId("search-panel-input").fill(query);
  await page.keyboard.press("Enter");
}

for (const language of LOCALES) {
  for (const skin of SKINS) {
    test.describe(`locale smoke — ${language} — ${skin}`, () => {
      test("sign-in", async ({ page }) => {
        await boot(page, skin, language, false);
        await expect(page.getByTestId("email-input")).toBeVisible();
        await audit(page, language, skin, "sign-in");
      });

      test("chat list, conversation, composer, settings, language, PIN", async ({ page }) => {
        await boot(page, skin, language, true);
        await expect(page.getByTestId("sidebar")).toBeVisible();
        await audit(page, language, skin, "chat-list");

        // Channel names are data, so the search query is the same in every locale.
        await searchAndOpen(page, "general");
        await expect(page.getByTestId(`message-${MESSAGE_ID}`)).toBeVisible();
        await audit(page, language, skin, "conversation");

        const composer = page.getByTestId("message-input");
        await composer.click();
        await composer.pressSequentially("hi");
        await audit(page, language, skin, "composer");

        // The sidebar row, not search: page keywords are translated and need not keep the English word.
        await page.getByTestId("sidebar-row-preferences").click();
        await expect(page.getByTestId("preferences-page")).toBeVisible();
        // One Language row naming the detected language (#1944); the list is its own page.
        await expect(page.getByTestId("pref-language-row").locator(`[lang="${language}"]`)).toBeVisible();
        await audit(page, language, skin, "settings");

        await page.getByTestId("pref-language-row").click();
        await expect(page.getByTestId("language-page")).toBeVisible();
        await expect(page.getByTestId(`pref-language-${language}`)).toHaveAttribute("aria-checked", "true");
        await audit(page, language, skin, "language");

        await page.evaluate(() => {
          (window as unknown as { __tauriEmit: (e: string) => void }).__tauriEmit("auto-lock");
        });
        await expect(page.getByTestId("pin-entry-screen")).toBeVisible();
        await audit(page, language, skin, "pin");
      });
    });
  }
}
