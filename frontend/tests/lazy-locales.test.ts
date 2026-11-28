// Lazy locales (#2186): a language chunk that fails or hangs never blocks start-up or sign-in (both
// wait on it, and the catch-up sweep and outbox flush start only after them); the app shows English.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import i18next from "i18next";
import { createLanguageLoader, type LoadLanguage } from "../src/i18n/languageLoader.ts";

const EN = { common: { hello: "Hello" }, chat: { reply: "Reply" } };
const FR = { common: { hello: "Bonjour" }, chat: { reply: "Répondre" } };

/** An instance set up like src/i18n/index.ts: English bundled, `lng` possibly not loaded yet. */
async function instance(lng = "en") {
  const i18n = i18next.createInstance();
  await i18n.init({
    resources: { en: EN },
    partialBundledLanguages: true,
    lng,
    fallbackLng: "en",
    supportedLngs: ["en", "fr", "ar"],
    ns: ["common", "chat"],
    defaultNS: "common",
    interpolation: { escapeValue: false },
  });
  return i18n;
}

const quiet = () => {};
const loadsFrench: LoadLanguage = async (language) => (language === "fr" ? FR : {});

test("start-up in a stored non-English language resolves to it, with its bundle present", async () => {
  // As at launch: init picks the stored language before its chunk is in, so resolvedLanguage is "en".
  const i18n = await instance("fr");
  assert.equal(i18n.resolvedLanguage, "en");
  const { switchLanguage } = createLanguageLoader(i18n, loadsFrench, { fallbackLanguage: "en", warn: quiet });
  // i18nReady = switchLanguage(i18n.language).
  await switchLanguage(i18n.language);
  assert.equal(i18n.language, "fr");
  assert.equal(i18n.resolvedLanguage, i18n.language);
  assert.ok(i18n.hasResourceBundle("fr", "common"));
  assert.equal(i18n.t("chat:reply"), "Répondre");
});

test("a chunk that rejects: start-up and sign-in still resolve, in English", async () => {
  const i18n = await instance("fr");
  let calls = 0;
  const rejects: LoadLanguage = async () => {
    calls++;
    throw new Error("Failed to fetch dynamically imported module");
  };
  const { switchLanguage, ensureLanguage } = createLanguageLoader(i18n, rejects, { fallbackLanguage: "en", warn: quiet });
  // i18nReady.
  await switchLanguage(i18n.language);
  assert.equal(i18n.resolvedLanguage, "en");
  assert.equal(i18n.language, "en");
  assert.equal(i18n.t("hello"), "Hello");
  // adoptUserLanguage at sign-in: resolves, English.
  await switchLanguage("fr");
  assert.equal(i18n.resolvedLanguage, "en");
  assert.equal(await ensureLanguage("fr"), false);
  // Each attempt retries: a failed load is not cached.
  assert.equal(calls, 3);
});

test("a chunk that throws synchronously still resolves, in English", async () => {
  const i18n = await instance();
  const throws = (() => {
    throw new Error("boom");
  }) as unknown as LoadLanguage;
  const { switchLanguage } = createLanguageLoader(i18n, throws, { fallbackLanguage: "en", warn: quiet });
  await switchLanguage("fr");
  assert.equal(i18n.resolvedLanguage, "en");
});

test("a chunk that never settles is bounded by the timeout, then English", async () => {
  const i18n = await instance("fr");
  const hangs: LoadLanguage = () => new Promise(() => {});
  const { switchLanguage } = createLanguageLoader(i18n, hangs, { fallbackLanguage: "en", timeoutMs: 50, warn: quiet });
  const started = Date.now();
  await switchLanguage(i18n.language);
  assert.ok(Date.now() - started < 1000);
  assert.equal(i18n.resolvedLanguage, "en");
});

test("a language with no chunk falls back to English", async () => {
  const i18n = await instance();
  const { switchLanguage } = createLanguageLoader(i18n, loadsFrench, { fallbackLanguage: "en", warn: quiet });
  await switchLanguage("ar");
  assert.equal(i18n.resolvedLanguage, "en");
});

test("a switch loads the language once, then t() resolves it; English needs no load", async () => {
  const i18n = await instance();
  let loads = 0;
  const counting: LoadLanguage = async (language) => {
    loads++;
    return loadsFrench(language);
  };
  const { switchLanguage } = createLanguageLoader(i18n, counting, { fallbackLanguage: "en", warn: quiet });
  await Promise.all([switchLanguage("fr"), switchLanguage("fr")]);
  assert.equal(loads, 1);
  assert.equal(i18n.t("hello"), "Bonjour");
  await switchLanguage("en");
  await switchLanguage("fr");
  assert.equal(loads, 1);
  assert.equal(i18n.resolvedLanguage, "fr");
});

test("a bundled partial locale (the Playwright test locale) switches without a load", async () => {
  const i18n = await instance();
  i18n.addResourceBundle("fr", "chat", { reply: "Répondre" });
  const never: LoadLanguage = async () => {
    throw new Error("must not load");
  };
  const { switchLanguage } = createLanguageLoader(i18n, never, { fallbackLanguage: "en", warn: quiet });
  await switchLanguage("fr");
  assert.equal(i18n.language, "fr");
  assert.equal(i18n.t("chat:reply"), "Répondre");
  // Missing keys still fall back to English.
  assert.equal(i18n.t("hello"), "Hello");
});

test("i18nReady, setLanguage and adoptUserLanguage go through switchLanguage; nothing else calls changeLanguage", () => {
  const src = path.resolve(import.meta.dirname, "../src");
  const index = readFileSync(path.join(src, "i18n/index.ts"), "utf8");
  assert.match(index, /export const i18nReady[\s\S]*?switchLanguage\(i18n\.language\)/);
  assert.match(index, /export async function setLanguage[\s\S]*?await switchLanguage\(normalized\)/);
  assert.match(index, /export async function adoptUserLanguage[\s\S]*?await switchLanguage\(stored\)/);
  const offenders: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const file = path.join(dir, name);
      if (statSync(file).isDirectory()) {
        if (name !== "__mocks__" && name !== "locales") {
          walk(file);
        }
      } else if (/\.tsx?$/.test(name) && !file.endsWith(path.join("i18n", "languageLoader.ts"))) {
        if (/\.changeLanguage\(/.test(readFileSync(file, "utf8"))) {
          offenders.push(path.relative(src, file));
        }
      }
    }
  };
  walk(src);
  assert.deepEqual(offenders, []);
  // The first render waits on i18nReady with `finally`, never success only.
  assert.match(readFileSync(path.join(src, "main.tsx"), "utf8"), /i18nReady\.finally\(/);
});
