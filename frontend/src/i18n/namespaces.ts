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
  // Copy that exists only in the mobile app. It lives in this directory so the
  // translators, `i18n-check` and the plural rules cover it like everything
  // else; the desktop bundles it and never calls it.
  "mobile",
] as const;

export type Namespace = (typeof NAMESPACES)[number];

export const DEFAULT_NAMESPACE: Namespace = "common";
