// Localization (#855), both skins: a synthetic PARTIAL locale (`qtt`) proves switching and the English fallback.

import { test, expect, type Page } from "./lib/zero-loss";
import { skinAgnosticTest } from "./lib/skins";
import fs from "node:fs";
import path from "node:path";

const USER = { id: "u-alice", email: "alice@example.com", username: "alice" };

const GROUP_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GRP";
const CHANNEL_ID = "01HQ7Z3K9M2P5R8T1V4W6Y0XCB";

// A private-use subtag (BCP-47 `qtt`), so it never collides with a real locale.
const TEST_LANG = "qtt";
const TEST_LANG_LABEL = "Tèstish";
const TEST_HEADING = "IDIOMA-TEST";

// `heading` is translated by the injected catalogue; `description` is not (proves the fallback).
const EN_HEADING = "Language";
const EN_DESCRIPTION_FRAGMENT = "Only changes this device";

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

/** Only `settings.language.heading` is translated, so "missing key" and "missing namespace" differ. */
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

/** Boot the app; `osLanguages` seeds navigator.languages for the first-run default. */
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
    /** A catalogue other than TEST_LOCALE, so its one-key gaps stay intact. */
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

/** Go to Preferences via Cmd+K (proves it is still in PAGE_RESULTS). */
async function gotoPreferences(page: Page) {
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+KeyK" : "Control+KeyK",
  );
  await expect(page.getByTestId("search-panel")).toBeVisible();
  // "appearance", not "Preferences": the Settings hub sorts ahead for the obvious query.
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

/** Rendered text or a11y attribute that is one of our real catalogue keys. */
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
  const agnosticTest = skinAgnosticTest(skin);
  test.describe(`i18n — ${skin} skin`, () => {
    agnosticTest("the language selector renders every available language", async ({ page }) => {
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

    agnosticTest("switching language changes the rendered copy", async ({ page }) => {
      await boot(page, skin);
      await gotoPreferences(page);

      const heading = page.getByTestId("pref-language-heading");
      await expect(heading).toHaveText(EN_HEADING);

      await page.getByTestId(`pref-language-${TEST_LANG}`).click();

      // Asserted on the heading only: the English description below legitimately says "Language".
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

    agnosticTest("the language choice persists across a reload", async ({ page }) => {
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

    agnosticTest("first run defaults to the OS locale when we ship it", async ({ page }) => {
      // A fresh profile starts in the OS locale; `qtt-XX` must degrade to `qtt`, not English.
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

/** Every registered locale renders its own word for "Language" (zh-Hans once nearly shipped as all-English). */
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

// #902: four surfaces #855 left speaking the wrong language.

/** Any Arabic-script character. */
const ARABIC_SCRIPT = /[؀-ۿ]/;
const LATIN_LETTER = /[A-Za-z]/;

/** Dates follow the APP language, not the (English-pinned) browser locale; asserted on script, not strings. */
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

/** Keyboard glyphs that are words are copy (synthetic locale: shipped ones lack these keys). */
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

/** Roles are translated, not echoed from the wire (synthetic: English copy equals the wire value). */
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

  /** Members is a parameterized route, not in PAGE_RESULTS: go via the sidebar's group menu. */
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

// Navigation must not depend on a translated label (#932): settings rows are found by testid.
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
      // …and its text really changed, so a label selector would miss it.
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
