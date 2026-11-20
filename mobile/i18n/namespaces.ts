/** Mirror of desktop's namespaces.ts; tests/i18n.test.ts fails if it drifts. */

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
  "mobile",
] as const;

export type Namespace = (typeof NAMESPACES)[number];

export const DEFAULT_NAMESPACE: Namespace = "common";
