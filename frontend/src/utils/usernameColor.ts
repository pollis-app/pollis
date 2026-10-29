import { useSyncExternalStore } from "react";

/** Stable per-user author colour (FNV-1a hue, memoised), with lightness chosen against the background. */

// Background lightness as a tiny external store for `useBackgroundIsLight`.

// Default from index.css (`--bg-l: 4%`).
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

/** Called by `applyBackgroundColor`; both variants are cached, so nothing to invalidate. */
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
    () => false,
  );
}

function hashString(str: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const cache = new Map<string, string>();

/** Stable HSL colour for `key` against the current background. */
export function getUsernameColor(key: string, isLightBg: boolean): string {
  const cacheKey = `${isLightBg ? "L" : "D"}:${key}`;
  const hit = cache.get(cacheKey);
  if (hit !== undefined) {
    return hit;
  }
  const hue = hashString(key) % 360;
  // Moderate saturation; lightness flips for ~4.5:1 contrast.
  const s = 65;
  const l = isLightBg ? 35 : 68;
  const color = `hsl(${hue} ${s}% ${l}%)`;
  cache.set(cacheKey, color);
  return color;
}

/** Admin name-chip fill (#1488): always the bright variant, which keeps dark `text-ink` ≥5:1. */
export function getUsernameChipColor(key: string): string {
  return getUsernameColor(key, false);
}
