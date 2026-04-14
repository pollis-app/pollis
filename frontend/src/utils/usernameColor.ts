import { useSyncExternalStore } from "react";

/**
 * Stable per-user color for message authors.
 *
 * - Deterministic: same `key` → same hue on every device and every render.
 * - Accessible: lightness flips based on the current background — bright
 *   saturated colors on dark bg, darker muted colors on light bg.
 * - Fast: O(n) FNV-1a hash over the key string (short usernames / ids),
 *   then the result is memoised forever in a module-level cache keyed on
 *   (key, bg-variant). Rendering hundreds of messages is a series of
 *   Map.get() hits after the first pass.
 */

// ── bg-lightness store ────────────────────────────────────────────────────
// We need to know whether the current background is light or dark so we
// can pick contrasting colors. Exposed as a tiny external store so
// components using `useBackgroundIsLight` re-render when the user
// changes their background preference.

// Default from index.css (`--bg-l: 4%`). Overwritten once the DOM is
// readable, and again on every `applyBackgroundColor` call.
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
 * Called from `applyBackgroundColor` whenever the user's background-color
 * preference resolves or changes. Notifies `useBackgroundIsLight`
 * subscribers so message rows re-render against the new contrast target.
 * The cache holds both light and dark variants keyed separately, so no
 * invalidation is required.
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
    // SSR fallback — assume dark so initial paint matches the default theme.
    () => false,
  );
}

// ── hash + color derivation ───────────────────────────────────────────────

// FNV-1a 32-bit. Tiny, allocation-free, good enough distribution for
// bucketing into 360 hues.
function hashString(str: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const cache = new Map<string, string>();

/**
 * Return a stable HSL color for `key` that contrasts with the current
 * background. Pass `isLightBg` from `useBackgroundIsLight()` so the
 * caller re-renders when the user swaps themes.
 */
export function getUsernameColor(key: string, isLightBg: boolean): string {
  const cacheKey = `${isLightBg ? "L" : "D"}:${key}`;
  const hit = cache.get(cacheKey);
  if (hit !== undefined) {
    return hit;
  }
  const hue = hashString(key) % 360;
  // Saturation is kept moderate: too high reads as neon on either theme.
  // Lightness flips: bright on dark bg, darker on light bg, both chosen
  // to stay roughly at 4.5:1 contrast against typical theme bgs.
  const s = 65;
  const l = isLightBg ? 35 : 68;
  const color = `hsl(${hue} ${s}% ${l}%)`;
  cache.set(cacheKey, color);
  return color;
}
