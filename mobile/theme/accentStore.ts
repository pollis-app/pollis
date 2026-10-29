// The accent's device-local copy (#1447), read SYNCHRONOUSLY at load so the first frame paints it. Pure.

import { DEFAULT_ACCENT_HEX } from "./accents.ts";

// expo-secure-store keys may not contain ":".
export const ACCENT_STORE_KEY = "pollis.theme.accent";

export interface AccentBackend {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void | Promise<void>;
}

export function normalizeAccentHex(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }
  const v = value.trim().toLowerCase();
  const hex = v.startsWith("#") ? v.slice(1) : v;
  if (!/^[0-9a-f]{6}$/.test(hex)) {
    return null;
  }
  return `#${hex}`;
}

/** The stored accent or the default. Never throws: a broken store never costs the launch. */
export function loadBootAccent(backend: AccentBackend | null | undefined): string {
  if (!backend) {
    return DEFAULT_ACCENT_HEX;
  }
  try {
    return normalizeAccentHex(backend.getItem(ACCENT_STORE_KEY)) ?? DEFAULT_ACCENT_HEX;
  } catch {
    return DEFAULT_ACCENT_HEX;
  }
}

export function persistAccent(backend: AccentBackend | null | undefined, hex: string): void {
  const normalized = normalizeAccentHex(hex);
  if (!backend || !normalized) {
    return;
  }
  try {
    const pending = backend.setItem(ACCENT_STORE_KEY, normalized);
    if (pending && typeof (pending as Promise<void>).catch === "function") {
      (pending as Promise<void>).catch(() => {});
    }
  } catch {
  }
}

/** `{}` before the unlock means "unread", not "default": only a `trusted` (post-unlock) blob resets to the default. */
export function accentFromPreferences(
  prefs: { accent_hex?: unknown } | null | undefined,
  trusted: boolean,
): string | null {
  if (!prefs) {
    return null;
  }
  return normalizeAccentHex(prefs.accent_hex) ?? (trusted ? DEFAULT_ACCENT_HEX : null);
}

/** Back to the default on sign-out, so the next account does not open in the last one's colour. */
export function accentOnUserChange(
  previousUserId: string | null,
  nextUserId: string | null,
): string | null {
  if (previousUserId && !nextUserId) {
    return DEFAULT_ACCENT_HEX;
  }
  return null;
}
