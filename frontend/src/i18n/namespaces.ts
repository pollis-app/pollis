/**
 * Translation namespaces, one JSON file each (`locales/<lng>/<ns>.json`), keys
 * as `ns:section.item`. `common` is the default, for shared verbs and nouns.
 */
export const NAMESPACES = [
  "common",
  "auth",
  "nav",
  "chat",
  "channels",
  "dms",
  "voice",
  "settings",
  "search",
  "emoji",
  "saved",
  "vault",
  "errors",
  "arcade",
  // Mobile-only copy, here so translators and `i18n-check` cover it.
  "mobile",
] as const;

export type Namespace = (typeof NAMESPACES)[number];

export const DEFAULT_NAMESPACE: Namespace = "common";
