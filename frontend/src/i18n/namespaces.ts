/**
 * Translation namespaces, one per feature area.
 *
 * A namespace is a JSON file under `locales/<lng>/<namespace>.json`. Keys are
 * addressed as `namespace:section.item` — e.g. `auth:login.emailLabel`.
 *
 * `common` is the default namespace: shared verbs and nouns that appear all
 * over the UI (Cancel, Save, Close…). Anything specific to one feature belongs
 * in that feature's namespace instead, so a translator working on voice never
 * has to read the settings catalogue.
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
] as const;

export type Namespace = (typeof NAMESPACES)[number];

export const DEFAULT_NAMESPACE: Namespace = "common";
