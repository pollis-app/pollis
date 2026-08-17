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
interface TestLocaleShape {
  code: string;
  label: string;
  dir: "ltr" | "rtl";
  catalogues: Record<string, Record<string, unknown>>;
}

async function boot(
  page: Page,
  skin: Skin,
  opts: {
    osLanguages?: string[];
    withTestLocale?: boolean;
    /** Extra preload state merged over the default fixture. */
    preload?: Record<string, unknown>;
    /**
     * A synthetic catalogue other than the module-level `TEST_LOCALE`. The
     * default one translates exactly ONE key on purpose — the tests below
     * that need a different key bring their own rather than widening it and
     * quietly weakening the fallback assertions that depend on its gaps.
     */
    locale?: TestLocaleShape;
  } = {},
) {
  const withTestLocale = opts.withTestLocale ?? true;
  const state = { ...preloadState(skin), ...(opts.preload ?? {}) };
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
      locale: withTestLocale ? (opts.locale ?? TEST_LOCALE) : null,
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

/*
 * ────────────────────────────────────────────────────────────────────────────
 * #902 — four surfaces #855 left speaking the wrong language.
 * ────────────────────────────────────────────────────────────────────────────
 */

/** Any Arabic-script character. */
const ARABIC_SCRIPT = /[؀-ۿ]/;
const LATIN_LETTER = /[A-Za-z]/;

/**
 * Dates must follow the APP language, not the OS one.
 *
 * `test.use({ locale })` pins the BROWSER's locale — i.e. the machine's — and
 * pinning it to English is the entire point of the fixture. `toLocaleDateString([])`
 * resolves against exactly that, so on an English machine the old code rendered
 * "Mon, Jun 7" underneath a translated "اليوم". A test that let the browser
 * locale follow the app language would pass on the unfixed code.
 *
 * The assertion is on SCRIPT, not on a specific string: the exact rendering of
 * a weekday is CLDR's business and changes between ICU versions, but "did any
 * Arabic come out at all" is the property under test and is stable.
 */
test.describe("dates follow the app language, not the OS locale", () => {
  test.use({ locale: "en-US" });

  const DAYS_AGO = 3;
  const DATED_MESSAGE_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0DTE";

  /** A message old enough for the day divider to take its weekday branch. */
  function datedMessages() {
    const sentAt = new Date(Date.now() - DAYS_AGO * 86_400_000).toISOString();
    return {
      messages: {
        [CHANNEL_ID]: [
          {
            id: DATED_MESSAGE_ID,
            conversation_id: CHANNEL_ID,
            sender_id: USER.id,
            content: "a message from earlier this week",
            sent_at: sentAt,
          },
        ],
      },
    };
  }

  async function gotoChannel(page: Page) {
    await page.keyboard.press(
      process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
    );
    await expect(page.getByTestId("search-panel")).toBeVisible();
    await page.getByTestId("search-panel-input").fill("general");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId(`message-${DATED_MESSAGE_ID}`)).toBeVisible();
  }

  test("an Arabic UI renders Arabic weekdays and months on an English machine", async ({
    page,
  }) => {
    await boot(page, "terminal", {
      withTestLocale: false,
      osLanguages: ["ar"],
      preload: datedMessages(),
    });
    // Precondition, not the test: if the locale failed to take, everything
    // below would be measuring an English app.
    await expect(page.locator("html")).toHaveAttribute("lang", "ar");
    await gotoChannel(page);

    const divider = page.getByTestId("day-divider").first();
    await expect(divider).toBeVisible();
    const label = (await divider.innerText()).trim();

    expect(label).toMatch(ARABIC_SCRIPT);
    // No Latin at all — this is the half that fails on the unfixed code, where
    // the weekday and month came back from the en-US host locale.
    expect(label).not.toMatch(LATIN_LETTER);

    // The hover tooltip is `formatFullTimestamp`, a separate `toLocaleString`
    // call site that had the same bug.
    const title = await page
      .getByTestId(`message-${DATED_MESSAGE_ID}`)
      .getByTestId("message-timestamp")
      .first()
      .getAttribute("title");
    expect(title ?? "").toMatch(ARABIC_SCRIPT);
  });

  test("an English UI still renders English dates", async ({ page }) => {
    // The other half of the pin. A one-sided assertion above is also satisfied
    // by a build that renders Arabic dates to everybody.
    await boot(page, "terminal", {
      withTestLocale: false,
      osLanguages: ["en-US"],
      preload: datedMessages(),
    });
    await expect(page.locator("html")).toHaveAttribute("lang", "en");
    await gotoChannel(page);

    const label = (await page.getByTestId("day-divider").first().innerText()).trim();
    expect(label).toMatch(LATIN_LETTER);
    expect(label).not.toMatch(ARABIC_SCRIPT);
  });
});

/**
 * Keyboard glyphs that are WORDS are copy.
 *
 * Driven through a synthetic locale rather than a real one because the six
 * shipped locales have not been translated for these keys yet (#902 ships
 * English only, by design) — so against `ar` this would assert the English
 * fallback and prove nothing. The synthetic catalogue is the same mechanism
 * the tests above use.
 */
test.describe("keyboard glyphs are translated", () => {
  const KEYS_LANG = "qtk";
  const ESC_TRANSLATED = "ESC-TEST";
  const CTRL_TRANSLATED = "CTRL-TEST";
  const SHIFT_TRANSLATED = "SHIFT-TEST";
  const SPACE_TRANSLATED = "SPACE-TEST";

  const KEYS_LOCALE: TestLocaleShape = {
    code: KEYS_LANG,
    label: "Kèyish",
    dir: "ltr",
    catalogues: {
      common: {
        keys: {
          escape: ESC_TRANSLATED,
          ctrl: CTRL_TRANSLATED,
          shift: SHIFT_TRANSLATED,
          space: SPACE_TRANSLATED,
        },
      },
    },
  };

  async function gotoShortcuts(page: Page) {
    await page.keyboard.press(
      process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
    );
    await expect(page.getByTestId("search-panel")).toBeVisible();
    await page.getByTestId("search-panel-input").fill("key bindings");
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("keyboard-shortcuts-page")).toBeVisible();
  }

  test("named keys render the active language, not English literals", async ({
    page,
  }) => {
    await boot(page, "terminal", {
      osLanguages: [KEYS_LANG],
      locale: KEYS_LOCALE,
    });
    await expect(page.locator("html")).toHaveAttribute("lang", KEYS_LANG);
    await gotoShortcuts(page);

    // `nav.back` is bound to a bare `escape`, so its badge is the key name on
    // its own — no platform-dependent modifier in the way.
    const escBadge = page.getByTestId("shortcut-nav.back-edit");
    await expect(escBadge).toHaveText(ESC_TRANSLATED);

    // Modifier NAMES are only rendered off macOS; there the glyphs ⌘/⇧ are
    // used, and those are deliberately never translated (see `keyCombo.ts`).
    if (process.platform !== "darwin") {
      const pttBadge = page.getByTestId("shortcut-voice.pushToTalk-edit");
      await expect(pttBadge).toHaveText(
        `${CTRL_TRANSLATED}+${SHIFT_TRANSLATED}+${SPACE_TRANSLATED}`,
      );
    }
  });

  test("English still renders the English key names", async ({ page }) => {
    await boot(page, "terminal", { osLanguages: ["en-US"], locale: KEYS_LOCALE });
    await expect(page.locator("html")).toHaveAttribute("lang", "en");
    await gotoShortcuts(page);
    await expect(page.getByTestId("shortcut-nav.back-edit")).toHaveText("Esc");
  });

});

/**
 * A member's role is a wire value, and the list rendered it straight to screen.
 *
 * Again synthetic, and for a sharper reason than usual: the English copy for
 * this key is deliberately the SAME WORD as the wire value it replaces
 * ("member"), so under any real locale — which falls back to English — a
 * reverted build renders an identical string and the test would pass on
 * unfixed code.
 */
test.describe("member roles are translated, not echoed from the wire", () => {
  const ROLE_LANG = "qtr";
  const MEMBER_TRANSLATED = "ROLE-MEMBER-TEST";
  const BOB = "u-bob";

  const ROLE_LOCALE: TestLocaleShape = {
    code: ROLE_LANG,
    label: "Rôlish",
    dir: "ltr",
    catalogues: {
      channels: { members: { role: { member: MEMBER_TRANSLATED } } },
    },
  };

  /** The signed-in user is NOT an admin, so every row shows its role. */
  const rosterPreload = {
    groups: [
      {
        id: GROUP_ID,
        name: "Acme",
        owner_id: BOB,
        created_at: new Date().toISOString(),
        current_user_role: "member",
      },
    ],
    groupMembers: {
      [GROUP_ID]: [
        { user_id: USER.id, username: "alice", role: "member", joined_at: "2026-08-01T09:00:00.000Z" },
        { user_id: BOB, username: "bob", role: "admin", joined_at: "2026-08-01T09:00:00.000Z" },
      ],
    },
  };

  /**
   * Members is a parameterized route, so it is not in `PAGE_RESULTS` and
   * cannot be reached from Cmd+K at all (the palette indexes channels, not
   * groups). Go the way a user does: the sidebar's group row opens the group
   * menu, and Members is on it.
   */
  async function gotoMembers(page: Page) {
    await page
      .getByTestId("sidebar")
      .getByRole("button", { name: "Acme", exact: true })
      .click();
    await page.getByTestId("menu-item-members").click();
    await expect(page.getByTestId("members-list")).toBeVisible();
  }

  test("the role column renders the active language", async ({ page }) => {
    await boot(page, "terminal", {
      osLanguages: [ROLE_LANG],
      locale: ROLE_LOCALE,
      preload: rosterPreload,
    });
    await expect(page.locator("html")).toHaveAttribute("lang", ROLE_LANG);
    await gotoMembers(page);

    const selfRow = page.getByTestId(`member-row-${USER.id}`);
    await expect(selfRow).toContainText(MEMBER_TRANSLATED);
    // The raw wire token is gone from that row.
    await expect(selfRow).not.toContainText(/\bmember\b/);
  });

  test("English still renders the English role label", async ({ page }) => {
    await boot(page, "terminal", {
      osLanguages: ["en-US"],
      locale: ROLE_LOCALE,
      preload: rosterPreload,
    });
    await gotoMembers(page);
    await expect(page.getByTestId(`member-row-${USER.id}`)).toContainText("member");
  });
});

/*
 * Navigation must not depend on a translated label (#932).
 *
 * The sidebar's settings rows are the only sidebar rows whose text comes from
 * a catalogue — groups, channels and DMs are labelled with user-generated
 * names. They carried no `data-testid`, so every spec that navigated to one
 * matched on its English label, and since #855 a locale change could therefore
 * break navigation tests for reasons that had nothing to do with navigation.
 * Worse, the failure reads as a routing bug rather than a selector bug.
 *
 * This is the assertion that pins it: under a locale that renames every one of
 * those rows, the testids still reach the pages.
 */
test.describe("sidebar settings rows are located by testid, not by label", () => {
  const NAV_LANG = "qtn";
  const NAV_TRANSLATED: Record<string, string> = {
    saved: "NAV-SAVED",
    preferences: "NAV-PREFERENCES",
    userSettings: "NAV-USER",
    voiceAndVideo: "NAV-VOICE",
    security: "NAV-SECURITY",
    keyBindings: "NAV-KEYS",
    softwareUpdate: "NAV-UPDATE",
  };

  const NAV_LOCALE: TestLocaleShape = {
    code: NAV_LANG,
    label: "Nàvish",
    dir: "ltr",
    catalogues: { nav: { sidebar: NAV_TRANSLATED } },
  };

  /** Row testid → the page testid clicking it must land on. */
  const ROWS: ReadonlyArray<readonly [string, string, string]> = [
    ["saved", "saved-page", "saved"],
    ["preferences", "preferences-page", "preferences"],
    ["user", "settings-page", "userSettings"],
    ["voice-settings", "voice-settings-page", "voiceAndVideo"],
    ["security", "security-page", "security"],
    ["shortcuts", "keyboard-shortcuts-page", "keyBindings"],
    ["update", "update-page", "softwareUpdate"],
  ];

  test("every settings row keeps its testid when its label is translated", async ({
    page,
  }) => {
    await boot(page, "terminal", { osLanguages: [NAV_LANG], locale: NAV_LOCALE });
    await expect(page.locator("html")).toHaveAttribute("lang", NAV_LANG);

    for (const [rowId, , labelKey] of ROWS) {
      const row = page.getByTestId(`sidebar-row-${rowId}`);
      // The row is findable by id…
      await expect(row).toBeVisible();
      // …and its text really did change, so a label-based selector would be
      // looking for a string that is no longer on the page. Without that, this
      // test would pass just as well in English and prove nothing.
      await expect(row).toContainText(NAV_TRANSLATED[labelKey]);
    }

    // The English labels the old selectors used are gone from the sidebar.
    const sidebar = page.getByTestId("sidebar");
    await expect(sidebar).not.toContainText("Preferences");
    await expect(sidebar).not.toContainText("Key Bindings");
  });

  test("each settings row still navigates to its page under a translated locale", async ({
    page,
  }) => {
    await boot(page, "terminal", { osLanguages: [NAV_LANG], locale: NAV_LOCALE });

    for (const [rowId, pageTestId] of ROWS) {
      await page.getByTestId(`sidebar-row-${rowId}`).click();
      await expect(page.getByTestId(pageTestId)).toBeVisible();
    }
  });

  test("the same testids work in English, so they are not a locale-only path", async ({
    page,
  }) => {
    await boot(page, "refined", { osLanguages: ["en-US"], locale: NAV_LOCALE });

    for (const [rowId, pageTestId] of ROWS) {
      await page.getByTestId(`sidebar-row-${rowId}`).click();
      await expect(page.getByTestId(pageTestId)).toBeVisible();
    }
  });
});
