/**
 * `t` for components mounted once per list row (#2167). react-i18next's `useTranslation` copies the
 * whole i18next instance on every mount; rows mount by the dozen on channel open and while scrolling,
 * so they share one fixed `t` per (language, namespace) instead.
 *
 * Re-renders on a language change and whenever a bundle is added (a lazily loaded catalogue, #2186):
 * the snapshot is a revision bumped on either, so a bundle that lands without a language change still
 * re-labels every row.
 */

import { useSyncExternalStore } from "react";
import type { TFunction } from "i18next";
import i18n from "./index";
import type { Namespace } from "./namespaces";

let revision = 0;
const listeners = new Set<() => void>();
let cache = new Map<string, TFunction>();

function bump(): void {
  revision += 1;
  // A new bundle may fill keys a cached `t` already resolved to English.
  cache = new Map();
  for (const listener of listeners) {
    listener();
  }
}

i18n.on("languageChanged", bump);
i18n.store.on("added", bump);

function subscribe(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

function currentRevision(): number {
  return revision;
}

export function useRowT(namespace: Namespace): TFunction {
  useSyncExternalStore(subscribe, currentRevision, currentRevision);
  const language = i18n.language;
  const key = `${language}|${namespace}`;
  let t = cache.get(key);
  if (!t) {
    t = i18n.getFixedT(language, namespace);
    cache.set(key, t);
  }
  return t;
}
