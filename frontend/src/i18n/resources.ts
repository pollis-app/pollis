/**
 * English, the fallback, is bundled eagerly. Every other language is its own local chunk
 * (`locale-<lng>`, vite.config.ts), loaded before first render and before a switch
 * (`languageLoader.ts`), so there is no untranslated flash; the chunks ship inside the signed app
 * bundle, so a load never touches the network. The mobile-only namespace is not bundled on desktop.
 * Listing a code in `languages.ts` makes it selectable.
 */

import { NAMESPACES } from "./namespaces";

type Catalogue = Record<string, unknown>;

const eagerModules = import.meta.glob<Catalogue>(
  ["./locales/en/*.json", "!**/mobile.json"],
  { eager: true, import: "default" },
);

const lazyModules = import.meta.glob<Catalogue>(
  ["./locales/*/*.json", "!**/en/*.json", "!**/mobile.json"],
  { import: "default" },
);

function parsePath(path: string): [string, string] | null {
  const match = /\.\/locales\/([^/]+)\/([^/]+)\.json$/.exec(path);
  if (!match) {
    return null;
  }
  const [, language, namespace] = match;
  if (!(NAMESPACES as readonly string[]).includes(namespace)) {
    console.warn(`[i18n] ignoring unknown namespace "${namespace}" in ${path}`);
    return null;
  }
  return [language, namespace];
}

/** The bundled catalogues: English only. */
export function buildResources(): Record<string, Record<string, Catalogue>> {
  const resources: Record<string, Record<string, Catalogue>> = {};
  for (const [path, catalogue] of Object.entries(eagerModules)) {
    const parsed = parsePath(path);
    if (!parsed) {
      continue;
    }
    const [language, namespace] = parsed;
    resources[language] ??= {};
    resources[language][namespace] = catalogue;
  }
  return resources;
}

/** Every namespace of one language from its chunk; empty for English (bundled) or an unknown code. */
export async function loadLanguageResources(language: string): Promise<Record<string, Catalogue>> {
  const loads: Promise<[string, Catalogue]>[] = [];
  for (const [path, load] of Object.entries(lazyModules)) {
    const parsed = parsePath(path);
    if (!parsed || parsed[0] !== language) {
      continue;
    }
    const namespace = parsed[1];
    loads.push(load().then((catalogue): [string, Catalogue] => [namespace, catalogue]));
  }
  return Object.fromEntries(await Promise.all(loads));
}
