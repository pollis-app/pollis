/* Only English and the active language are built at launch (#2187); a switch adds the new
 * language before it is active, and a language that cannot load leaves English. Runs the real
 * generated loader table against a real i18next, as i18n/index.ts wires them. */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import i18next from "i18next";
import { ensureLanguage, initialResources, loadLanguage, switchLanguage, type LanguageLoaders } from "../i18n/load.ts";
import { NAMESPACES, DEFAULT_NAMESPACE } from "../i18n/namespaces.ts";

// The generated table uses Metro's synchronous `require`; give node the same, rooted where the file is.
(globalThis as { require?: NodeJS.Require }).require = createRequire(new URL("../i18n/resources.ts", import.meta.url));
const { LANGUAGE_LOADERS } = await import("../i18n/resources.ts");

const MOBILE = new URL("..", import.meta.url).pathname;
const src = (p: string) => readFileSync(join(MOBILE, p), "utf8");

async function instance(loaders: LanguageLoaders, lng: string) {
  const i = i18next.createInstance();
  await i.init({
    resources: initialResources(loaders, "en", [lng]),
    lng,
    fallbackLng: "en",
    ns: NAMESPACES,
    defaultNS: DEFAULT_NAMESPACE,
    returnEmptyString: false,
    returnNull: false,
    interpolation: { escapeValue: false },
  });
  return i;
}

const FR_TODAY = JSON.parse(src("../frontend/src/i18n/locales/fr/common.json")).time.today as string;
const EN_TODAY = JSON.parse(src("../frontend/src/i18n/locales/en/common.json")).time.today as string;
const JA_TODAY = JSON.parse(src("../frontend/src/i18n/locales/ja/common.json")).time.today as string;

test("the table has a loader for every shipped language, and nothing is built until called", () => {
  const dirs = readdirSync(join(MOBILE, "../frontend/src/i18n/locales")).sort();
  assert.deepEqual(Object.keys(LANGUAGE_LOADERS).sort(), dirs);
  assert.ok(!/^import /m.test(src("i18n/resources.ts")), "no top-level catalogue import");
});

test("launch builds English and the active language only", async () => {
  const built: string[] = [];
  const spy: LanguageLoaders = Object.fromEntries(
    Object.entries(LANGUAGE_LOADERS).map(([lng, load]) => [lng, () => (built.push(lng), load())]),
  );
  const i = await instance(spy, "fr");
  assert.deepEqual(built.sort(), ["en", "fr"]);
  assert.equal(i.t("common:time.today"), FR_TODAY);
  assert.equal(i.resolvedLanguage, "fr");
  assert.equal(i.hasResourceBundle("ja", DEFAULT_NAMESPACE), false);
});

test("a switch makes t() and resolvedLanguage answer in the new language at once", async () => {
  const i = await instance(LANGUAGE_LOADERS, "en");
  assert.equal(i.t("common:time.today"), EN_TODAY);
  await switchLanguage(i, LANGUAGE_LOADERS, DEFAULT_NAMESPACE, "ja");
  assert.equal(i.language, "ja");
  assert.equal(i.resolvedLanguage, "ja");
  assert.equal(i.t("common:time.today"), JA_TODAY);
  // Every namespace came with it.
  for (const ns of NAMESPACES) {
    assert.ok(i.hasResourceBundle("ja", ns), `ja/${ns}`);
  }
});

test("a language whose catalogues fail to load shows English and never throws", async () => {
  const broken: LanguageLoaders = {
    ...LANGUAGE_LOADERS,
    fr: () => {
      throw new Error("corrupt catalogue");
    },
  };
  const warn = console.warn;
  console.warn = () => {};
  try {
    assert.equal(loadLanguage(broken, "fr"), null);
    const i = await instance(broken, "en");
    await switchLanguage(i, broken, DEFAULT_NAMESPACE, "fr");
    assert.equal(i.t("common:time.today"), EN_TODAY);
    // Launching in it falls back the same way.
    const j = await instance(broken, "fr");
    assert.equal(j.t("common:time.today"), EN_TODAY);
    // An unknown code too.
    assert.equal(ensureLanguage(j, broken, DEFAULT_NAMESPACE, "xx"), false);
  } finally {
    console.warn = warn;
  }
});

test("the headless push path's t() answers in the active language", async () => {
  // lib/push reads i18n.t for the Android channel name with no await: the active language's
  // catalogues are in the init resources, so it is translated from the first call.
  const i = await instance(LANGUAGE_LOADERS, "fr");
  const key = "mobile:push.channelMessages";
  const fr = JSON.parse(src("../frontend/src/i18n/locales/fr/mobile.json")).push?.channelMessages;
  assert.equal(typeof fr, "string");
  assert.equal(i.t(key), fr);
  assert.match(src("lib/push/index.ts"), /i18n\.t\("mobile:push\.channelMessages"\)/);
});

test("every language change goes through switchLanguage (the bundle is added first)", () => {
  const index = src("i18n/index.ts");
  assert.ok(!/i18n\.changeLanguage\(/.test(index), "i18n/index.ts calls switchLanguage, never changeLanguage");
  assert.equal(index.match(/switchLanguage\(i18n, LANGUAGE_LOADERS, DEFAULT_NAMESPACE, /g)?.length, 3);
  for (const fn of ["hydrateLanguage", "setLanguage", "adoptUserLanguage"]) {
    const body = index.slice(index.indexOf(`export async function ${fn}(`));
    const end = body.indexOf("\n}\n");
    assert.match(body.slice(0, end), /switchLanguage\(/, `${fn} switches through switchLanguage`);
  }
  // Nowhere else in the app.
  const offenders: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(join(MOBILE, dir), { withFileTypes: true })) {
      const p = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!["node_modules", "android", "ios", "tests", ".expo"].includes(entry.name)) {
          walk(p);
        }
      } else if (/\.(ts|tsx)$/.test(entry.name) && p !== "i18n/load.ts" && /\.changeLanguage\(/.test(src(p))) {
        offenders.push(p);
      }
    }
  };
  for (const dir of ["app", "components", "hooks", "lib", "i18n", "stores"]) {
    walk(dir);
  }
  assert.deepEqual(offenders, []);
});
