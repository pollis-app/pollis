/** Translation namespaces (`locales/<lng>/<ns>.json`, keys `ns:section.item`); `common` is the default. */
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
  // Mobile-only copy, kept here so i18n tooling covers it.
  "mobile",
] as const;

export type Namespace = (typeof NAMESPACES)[number];

export const DEFAULT_NAMESPACE: Namespace = "common";
