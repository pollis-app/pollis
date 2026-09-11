/**
 * The namespace list — the mobile mirror of `frontend/src/i18n/namespaces.ts`.
 * `tests/i18n.test.ts` fails if it drifts from the catalogue files that ship.
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
  "mobile",
] as const;

export type Namespace = (typeof NAMESPACES)[number];

export const DEFAULT_NAMESPACE: Namespace = "common";
