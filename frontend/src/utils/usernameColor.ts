import { useSyncExternalStore } from "react";

/**
 * Stable per-user author colour: deterministic per `key`, lightness flipped
 * for the background, memoised per (key, bg variant).
 */

// Background lightness as a tiny external store, so `useBackgroundIsLight`
// re-renders on a background change.

// index.css default (`--bg-l: 4%`), overwritten by `applyBackgroundColor`.
let currentBgL = 4;
let initialized = false;
const listeners = new Set<() => void>();

function emit(): void {
  listeners.forEach((cb) => cb());
}

function initFromDom(): void {
  if (initialized || typeof document === "undefined") {
    return;
  }
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue("--bg-l")
    .trim();
  const parsed = parseFloat(raw);
  if (!isNaN(parsed)) {
    currentBgL = parsed;
  }
  initialized = true;
}

/**
 * Called by `applyBackgroundColor`; notifies `useBackgroundIsLight`. Both
 * variants are cached separately, so nothing is invalidated.
 */
export function setBackgroundLightness(l: number): void {
  if (currentBgL === l && initialized) {
    return;
  }
  currentBgL = l;
  initialized = true;
  emit();
}

export function isBackgroundLight(): boolean {
  initFromDom();
  return currentBgL > 50;
}

export function useBackgroundIsLight(): boolean {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => {
        listeners.delete(cb);
      };
    },
    isBackgroundLight,
      // SSR: assume dark, matching the default theme.
    () => false,
  );
}

// FNV-1a 32-bit: allocation-free, enough for 360 hues.
function hashString(str: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const cache = new Map<string, string>();

/** A stable HSL colour for `key` against the current background (pass `isLightBg` from `useBackgroundIsLight()`). */
export function getUsernameColor(key: string, isLightBg: boolean): string {
  const cacheKey = `${isLightBg ? "L" : "D"}:${key}`;
  const hit = cache.get(cacheKey);
  if (hit !== undefined) {
    return hit;
  }
  const hue = hashString(key) % 360;
  // Moderate saturation (high reads as neon); lightness flips per theme for
  // roughly 4.5:1 contrast.
  const s = 65;
  const l = isLightBg ? 35 : 68;
  const color = `hsl(${hue} ${s}% ${l}%)`;
  cache.set(cacheKey, color);
  return color;
}

/**
 * Fill for another admin's name chip (#1488): always the bright variant, since
 * the chip's dark `text-ink` needs it (≥ 5:1 for every hue; the light variant
 * drops under 1.7:1). Pinned in `username-color.test.ts`.
 */
export function getUsernameChipColor(key: string): string {
  return getUsernameColor(key, false);
}
