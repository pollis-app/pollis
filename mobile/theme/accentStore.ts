// The colours' device-local copy (#1447), read SYNCHRONOUSLY at load so the first frame paints them. Pure.

import { DEFAULT_ACCENT_HEX, DEFAULT_BACKGROUND_HEX } from "./accents.ts";
import { accentFromPrefs, normalizeHex } from "../../design/theme.ts";

// expo-secure-store keys may not contain ":".
export const ACCENT_STORE_KEY = "pollis.theme.accent";
export const BACKGROUND_STORE_KEY = "pollis.theme.background";

export interface AccentBackend {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void | Promise<void>;
}

export function normalizeAccentHex(value: unknown): string | null {
  return normalizeHex(value);
}

function loadBootColor(backend: AccentBackend | null | undefined, key: string, fallback: string): string {
  if (!backend) {
    return fallback;
  }
  try {
    return normalizeAccentHex(backend.getItem(key)) ?? fallback;
  } catch {
    return fallback;
  }
}

function persistColor(backend: AccentBackend | null | undefined, key: string, hex: string): void {
  const normalized = normalizeAccentHex(hex);
  if (!backend || !normalized) {
    return;
  }
  try {
    const pending = backend.setItem(key, normalized);
    if (pending && typeof (pending as Promise<void>).catch === "function") {
      (pending as Promise<void>).catch(() => {});
    }
  } catch {
  }
}

/** The stored accent or the default. Never throws: a broken store never costs the launch. */
export function loadBootAccent(backend: AccentBackend | null | undefined): string {
  return loadBootColor(backend, ACCENT_STORE_KEY, DEFAULT_ACCENT_HEX);
}

export function persistAccent(backend: AccentBackend | null | undefined, hex: string): void {
  persistColor(backend, ACCENT_STORE_KEY, hex);
}

/** The stored background or the default; same rules as the accent. */
export function loadBootBackground(backend: AccentBackend | null | undefined): string {
  return loadBootColor(backend, BACKGROUND_STORE_KEY, DEFAULT_BACKGROUND_HEX);
}

export function persistBackground(backend: AccentBackend | null | undefined, hex: string): void {
  persistColor(backend, BACKGROUND_STORE_KEY, hex);
}

/** `{}` before the unlock means "unread", not "default": only a `trusted` (post-unlock) blob resets to the default. Reads the shared `accent_color`, then the legacy `accent_hex`. */
export function accentFromPreferences(
  prefs: { accent_color?: unknown; accent_hex?: unknown } | null | undefined,
  trusted: boolean,
): string | null {
  if (!prefs) {
    return null;
  }
  return accentFromPrefs(prefs) ?? (trusted ? DEFAULT_ACCENT_HEX : null);
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
