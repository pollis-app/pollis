/*
 * Localization (#855) — infrastructure e2e.
 *
 * Runs against the browser-rendered frontend with the Tauri IPC mock
 * (`frontend/src/__mocks__/tauri-core.ts`), same as the other browser-tier
 * specs.
 *
 * Phase 1 ships only the English catalogue, so "does switching language work"
 * is not assertable against a real locale yet. The spec therefore injects a
 * SYNTHETIC locale through `window.__POLLIS_TEST_LOCALE__`, which
 * `frontend/src/i18n/index.ts` registers only under `VITE_PLAYWRIGHT`. That
 * keeps these assertions true regardless of which real locales land in later
 * phases — a test that hardcoded "German" would start failing the day someone
 * reordered the language list.
 *
 * The injected catalogue is deliberately PARTIAL: it translates
 * `settings:language.heading` and nothing else. Everything the app renders
 * beyond that key must therefore still come out in English, which is exactly
 * the fallback guarantee this feature rests on.
 *
 * Every test runs in BOTH skins, per the repo convention that visual features
 * stay consistent across `terminal` and `refined`.
 *
 *   pnpm --filter @pollis/e2e playwright
 */

import { test, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

/*
 * A private-use language subtag (BCP-47 `qtt`), so it can never collide with
 * a real locale a later phase adds.
 */
const TEST_LANG = "qtt";
const TEST_LANG_LABEL = "Tèstish";
const TEST_HEADING = "IDIOMA-TEST";

/*
 * The English strings this spec pins. Both live in `settings:language.*`.
 * `heading` is translated by the injected catalogue; `description` is NOT,
 * and so proves the English fallback.
 */
const EN_HEADING = "Language";
const EN_DESCRIPTION_FRAGMENT = "per-device";

const SKINS = ["terminal", "refined"] as const;
type Skin = (typeof SKINS)[number];

function preloadState(skin: Skin) {
  return {
    session: USER,
    profile: { id: USER.id, username: USER.username },
    preferences: JSON.stringify({ skin }),
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: USER.id,
        created_at: new Date().toISOString(),
      },
    ],
    channels: {
      [GROUP_ID]: [{ id: CHANNEL_ID, group_id: GROUP_ID, name: "general" }],
    },
    messages: {},
    dmChannels: [],
  };
}

/**
 * The synthetic locale. Only `settings.language.heading` is translated —
 * `ariaLabel` and `description` are omitted on purpose, and every other
 * namespace is absent entirely, so the spec can tell "missing key" and
 * "missing namespace" apart.
 */
const TEST_LOCALE = {
  code: TEST_LANG,
  label: TEST_LANG_LABEL,
  dir: "ltr" as const,
  catalogues: {
    settings: {
      language: {
        heading: TEST_HEADING,
      },
    },
  },
};

/**
 * Boot the app.
 *
 * `osLanguages` seeds `navigator.languages` so the first-run OS-locale default
 * can be exercised; it must be installed before any app module evaluates,
 * hence `addInitScript`.
 */
async function boot(
  page: Page,
  skin: Skin,
  opts: { osLanguages?: string[]; withTestLocale?: boolean } = {},
) {
  const withTestLocale = opts.withTestLocale ?? true;
  const state = preloadState(skin);
  await page.addInitScript(
    ({ preload, locale, osLanguages }) => {
      const w = window as unknown as Record<string, unknown>;
      w.__POLLIS_PRELOAD__ = preload;
      if (locale) {
        w.__POLLIS_TEST_LOCALE__ = locale;
      }
      if (osLanguages) {
        Object.defineProperty(navigator, "languages", {
          get: () => osLanguages,
          configurable: true,
        });
        Object.defineProperty(navigator, "language", {
          get: () => osLanguages[0],
          configurable: true,
        });
      }
      // `@tauri-apps/api/window` is NOT vite-aliased (only `/core` and
      // `/event` are), so the real module runs and reads `__TAURI_INTERNALS__`
      // directly. It must exist before any app module evaluates.
      w.__TAURI_INTERNALS__ = {
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
    },
    {
      preload: state,
      locale: withTestLocale ? TEST_LOCALE : null,
      osLanguages: opts.osLanguages ?? null,
    },
  );
  await page.goto("/");
  await expect(page.getByTestId("sidebar")).toBeVisible();
}

/**
 * Navigate to Preferences through Cmd+K.
 *
 * Deliberately not a direct URL visit: the router runs on `createMemoryHistory`
 * (see `frontend/src/router.tsx`) so the browser URL is meaningless, and going
 * through the palette also proves the page is still registered in
 * `PAGE_RESULTS` after the search labels were moved into a catalogue.
 */
async function gotoPreferences(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  // "appearance" rather than "Preferences": the Settings hub lists
  // "preferences" among its own keywords and sorts ahead of the Preferences
  // page, so the obvious query lands on the wrong screen. "appearance" is
  // unique to the Preferences entry.
  await page.getByTestId("search-panel-input").fill("appearance");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("preferences-page")).toBeVisible();
}

const EN_LOCALE_DIR = path.join(
  __dirname,
  "..",
  "frontend",
  "src",
  "i18n",
  "locales",
  "en",
);

function collectKeyPaths(
  node: unknown,
  prefix: string,
  namespace: string,
  out: Set<string>,
): void {
  if (node === null || typeof node !== "object") {
    return;
  }
  for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
    const dotted = prefix ? `${prefix}.${key}` : key;
    if (typeof value === "object" && value !== null) {
      collectKeyPaths(value, dotted, namespace, out);
      continue;
    }
    out.add(dotted);
    out.add(`${namespace}:${dotted}`);
    // i18next resolves a plural key by appending a CLDR suffix, so the key a
    // caller passes — and therefore the key that would leak — is the stem.
    const stem = dotted.replace(/_(zero|one|two|few|many|other)$/, "");
    if (stem !== dotted) {
      out.add(stem);
      out.add(`${namespace}:${stem}`);
    }
  }
}

/**
 * Every key path that exists in the English catalogues, in both the bare
 * (`language.heading`) and namespaced (`settings:language.heading`) forms
 * i18next echoes back when a lookup misses.
 */
function englishKeyPaths(): string[] {
  const out = new Set<string>();
  for (const file of fs.readdirSync(EN_LOCALE_DIR)) {
    if (!file.endsWith(".json")) {
      continue;
    }
    const namespace = file.replace(/\.json$/, "");
    const parsed: unknown = JSON.parse(
      fs.readFileSync(path.join(EN_LOCALE_DIR, file), "utf8"),
    );
    collectKeyPaths(parsed, "", namespace, out);
  }
  return [...out];
}

/**
 * Rendered text that is actually one of our translation keys.
 *
 * Matching against the real catalogue rather than a "looks dotted" regex
 * matters both ways round: it cannot be fooled by legitimate dotted content
 * the app renders (hostnames, filenames, version strings), and it cannot
 * silently stop working when the catalogues grow — a key added tomorrow is
 * covered the moment it exists.
 *
 * Covers `aria-label`, `title`, `placeholder` and `alt` as well as text
 * nodes; roughly 150 of the extracted strings live in attributes, and a key
 * leaking into a screen-reader label is no less broken for being invisible.
 */
async function rawKeyLeaks(page: Page): Promise<string[]> {
  return page.evaluate((keyPaths: string[]) => {
    const keys = new Set(keyPaths);
    const leaks = new Set<string>();

    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = (node.textContent ?? "").trim();
      if (text && keys.has(text)) {
        leaks.add(text);
      }
    }

    for (const attr of ["aria-label", "title", "placeholder", "alt"]) {
      for (const el of document.querySelectorAll(`[${attr}]`)) {
        const value = (el.getAttribute(attr) ?? "").trim();
        if (value && keys.has(value)) {
          leaks.add(`[${attr}] ${value}`);
        }
      }
    }

    return [...leaks];
  }, englishKeyPaths());
}

for (const skin of SKINS) {
  test.describe(`i18n — ${skin} skin`, () => {
    test("the language selector renders every available language", async ({ page }) => {
      await boot(page, skin);
      await gotoPreferences(page);

      const section = page.getByTestId("pref-language");
      await expect(section).toBeVisible();
      await expect(page.getByTestId("pref-language-heading")).toHaveText(EN_HEADING);

      // English always ships; the synthetic locale is registered on top.
      await expect(page.getByTestId("pref-language-en")).toBeVisible();
      await expect(page.getByTestId(`pref-language-${TEST_LANG}`)).toBeVisible();

      // Languages are labelled with their own name for themselves.
      await expect(page.getByTestId(`pref-language-${TEST_LANG}`)).toHaveText(
        TEST_LANG_LABEL,
      );

      // English is active by default, and the active option is the primary one.
      await expect(page.getByTestId("pref-language-en")).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await expect(page.getByTestId(`pref-language-${TEST_LANG}`)).toHaveAttribute(
        "aria-pressed",
        "false",
      );
    });

    test("switching language changes the rendered copy", async ({ page }) => {
      await boot(page, skin);
      await gotoPreferences(page);

      const heading = page.getByTestId("pref-language-heading");
      await expect(heading).toHaveText(EN_HEADING);

      await page.getByTestId(`pref-language-${TEST_LANG}`).click();

      // The one key the synthetic catalogue translates. Asserted on the
      // heading element specifically — the English description below it
      // legitimately contains the word "Language" via the fallback, so a
      // section-wide `not.toContainText` would be asserting the opposite of
      // what this feature promises.
      await expect(heading).toHaveText(TEST_HEADING);

      // Selection moved with it.
      await expect(page.getByTestId(`pref-language-${TEST_LANG}`)).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await expect(page.getByTestId("pref-language-en")).toHaveAttribute(
        "aria-pressed",
        "false",
      );

      // `<html lang>` follows, which is what drives hyphenation, spellcheck
      // and the screen-reader voice.
      await expect(page.locator("html")).toHaveAttribute("lang", TEST_LANG);
    });

    test("a missing key falls back to English instead of rendering the key", async ({ page }) => {
      await boot(page, skin);
      await gotoPreferences(page);
      await page.getByTestId(`pref-language-${TEST_LANG}`).click();
      await expect(page.getByTestId("pref-language-heading")).toHaveText(TEST_HEADING);

      // `settings:language.description` exists in the namespace the synthetic
      // locale DOES provide, but is absent from it — so it must render the
      // English string.
      await expect(page.getByTestId("pref-language")).toContainText(
        EN_DESCRIPTION_FRAGMENT,
      );

      // `settings:language.ariaLabel` is likewise absent.
      await expect(
        page.getByTestId("pref-language").getByRole("radiogroup"),
      ).toHaveAttribute("aria-label", EN_HEADING);

      // And nothing anywhere in the tree — including the many namespaces the
      // synthetic locale omits ENTIRELY — leaked a raw dotted key.
      expect(await rawKeyLeaks(page)).toEqual([]);
    });

    test("the language choice persists across a reload", async ({ page }) => {
      await boot(page, skin);
      await gotoPreferences(page);
      await page.getByTestId(`pref-language-${TEST_LANG}`).click();
      await expect(page.getByTestId("pref-language-heading")).toHaveText(TEST_HEADING);

      await page.reload();
      await expect(page.getByTestId("sidebar")).toBeVisible();

      // Restored before any UI is touched — `<html lang>` is set during init.
      await expect(page.locator("html")).toHaveAttribute("lang", TEST_LANG);

      await gotoPreferences(page);
      await expect(page.getByTestId("pref-language-heading")).toHaveText(TEST_HEADING);
      await expect(page.getByTestId(`pref-language-${TEST_LANG}`)).toHaveAttribute(
        "aria-pressed",
        "true",
      );
    });

    test("first run defaults to the OS locale when we ship it", async ({ page }) => {
      // The synthetic locale stands in for "a locale we ship". A fresh profile
      // with no stored choice should start there. `-XX` also exercises the
      // region-subtag degrade: `qtt-XX` has no catalogue of its own but must
      // still land on `qtt` rather than dropping to English.
      await boot(page, skin, { osLanguages: [`${TEST_LANG}-XX`, "en-US"] });
      await expect(page.locator("html")).toHaveAttribute("lang", TEST_LANG);
      await gotoPreferences(page);
      await expect(page.getByTestId("pref-language-heading")).toHaveText(TEST_HEADING);
    });

    test("first run falls back to English for a locale we do not ship", async ({
      page,
    }) => {
      await boot(page, skin, { osLanguages: ["xx-YY", "zz"] });
      await expect(page.locator("html")).toHaveAttribute("lang", "en");
      await gotoPreferences(page);
      await expect(page.getByTestId("pref-language-heading")).toHaveText(EN_HEADING);
      expect(await rawKeyLeaks(page)).toEqual([]);
    });
  });
}

/**
 * Every SHIPPED locale actually renders — not just the synthetic one.
 *
 * The tests above all drive `qtt`, which proves the machinery works but says
 * nothing about the real catalogues. That gap is not theoretical: Simplified
 * Chinese was very nearly shipped as `zh-Hans`, a tag this app's own
 * `normalizeLanguage` and i18next's canonicalization reject from opposite
 * ends. The catalogue would have been complete, the key validator green, the
 * type checker happy, and every user would have seen English.
 *
 * So this walks the registry itself rather than a hardcoded list — a locale
 * added later is covered the moment it is registered — and asserts each one
 * renders its OWN word for "Language", which is distinct in all seven.
 */
const SHIPPED = JSON.parse(
  JSON.stringify(
    (() => {
      const source = fs.readFileSync(
        path.join(__dirname, "../frontend/src/i18n/languages.ts"),
        "utf8",
      );
      const block = source.slice(
        source.indexOf("SHIPPED_LANGUAGES: readonly"),
        source.indexOf("];", source.indexOf("SHIPPED_LANGUAGES: readonly")),
      );
      return [...block.matchAll(/\{\s*code:\s*"([\w-]+)".*?dir:\s*"(ltr|rtl)"/g)].map(
        ([, code, dir]) => ({ code, dir }),
      );
    })(),
  ),
) as Array<{ code: string; dir: "ltr" | "rtl" }>;

test.describe("shipped locales", () => {
  test("the registry is the seven we intend to ship", () => {
    expect(SHIPPED.map((l) => l.code)).toEqual([
      "en",
      "es",
      "uk",
      "fr",
      "ru",
      "zh",
      "ar",
    ]);
  });

  for (const { code, dir } of SHIPPED.filter((l) => l.code !== "en")) {
    test(`${code} renders its own catalogue rather than falling back`, async ({
      page,
    }) => {
      // Read the expectation from the catalogue on disk, so this cannot drift
      // from the translation and cannot be satisfied by a stale literal.
      const heading = JSON.parse(
        fs.readFileSync(
          path.join(
            __dirname,
            `../frontend/src/i18n/locales/${code}/settings.json`,
          ),
          "utf8",
        ),
      ).language.heading as string;
      expect(heading).not.toBe(EN_HEADING);

      await boot(page, "terminal");
      await gotoPreferences(page);
      await page.getByTestId(`pref-language-${code}`).click();

      await expect(page.getByTestId("pref-language-heading")).toHaveText(heading);
      await expect(page.locator("html")).toHaveAttribute("lang", code);
      // Direction travels with the language; Arabic is the only rtl entry.
      await expect(page.locator("html")).toHaveAttribute("dir", dir);
      // A locale that silently half-loaded would leak raw keys somewhere.
      expect(await rawKeyLeaks(page)).toEqual([]);
    });
  }
});
