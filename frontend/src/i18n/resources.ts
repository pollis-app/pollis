/**
 * Catalogue loading.
 *
 * Every `locales/<lng>/<namespace>.json` is bundled eagerly. Bundle size is
 * explicitly not a concern in this repo, and eager loading buys two things
 * that a lazy backend does not: no untranslated flash on first paint, and no
 * async failure mode on a machine that is offline (which, for a desktop app
 * that has to work offline, is not an edge case).
 *
 * Consequence for anyone adding a locale: dropping the JSON files in is
 * enough for them to be *loadable*. Listing the code in `SUPPORTED_LANGUAGES`
 * is what makes it *selectable*.
 */

import { NAMESPACES } from "./namespaces";

type Catalogue = Record<string, unknown>;

const modules = import.meta.glob<Catalogue>("./locales/*/*.json", {
  eager: true,
  import: "default",
});

/**
 * `{ en: { common: {...}, auth: {...} }, … }` — the shape i18next's
 * `resources` option expects.
 */
export function buildResources(): Record<string, Record<string, Catalogue>> {
  const resources: Record<string, Record<string, Catalogue>> = {};
  for (const [path, catalogue] of Object.entries(modules)) {
    const match = /\.\/locales\/([^/]+)\/([^/]+)\.json$/.exec(path);
    if (!match) {
      continue;
    }
    const [, language, namespace] = match;
    if (!(NAMESPACES as readonly string[]).includes(namespace)) {
      console.warn(`[i18n] ignoring unknown namespace "${namespace}" in ${path}`);
      continue;
    }
    resources[language] ??= {};
    resources[language][namespace] = catalogue;
  }
  return resources;
}
