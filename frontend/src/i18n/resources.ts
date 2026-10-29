/** Every catalogue bundled eagerly: no untranslated flash, no offline failure. Listing a code makes it selectable. */

import { NAMESPACES } from "./namespaces";

type Catalogue = Record<string, unknown>;

const modules = import.meta.glob<Catalogue>("./locales/*/*.json", {
  eager: true,
  import: "default",
});

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
