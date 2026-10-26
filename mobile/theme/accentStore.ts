// The accent's device-local copy (#1447). Pure (no react-native) so
// tests/theme-startup.test.ts can drive it with a fake store.
//
// The user's accent is a synced preference (`accent_hex` in the preferences
// blob), but that blob needs a signed-in user and a DS round trip, so it lands
// well after the first frame. Until then every screen (the PIN screen
// included) drew the default amber. The device therefore keeps the last
// accent it showed in a store it can read SYNCHRONOUSLY, and theme/tokens.ts
// reads it while the module loads, before anything renders. The synced value
// still wins once it arrives (`accentFromPreferences`), and is written back
// here so the next cold start opens in it.

import { DEFAULT_ACCENT_HEX } from "./accents.ts";

// expo-secure-store keys may not contain ":".
export const ACCENT_STORE_KEY = "pollis.theme.accent";

// The two calls the accent needs from a key-value store. Reads must be
// synchronous: an async read would land after the first frame.
export interface AccentBackend {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void | Promise<void>;
}

/** "#rrggbb" (any case, `#` optional) → "#rrggbb" lowercase; anything else → null. */
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

/**
 * The accent to paint the first frame with: the stored one, or the default
 * when there is none, it is malformed, or the store cannot be read. Never
 * throws — a broken store costs the user their colour, never the launch.
 */
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

/** Remembers `hex` for the next cold start. Best effort, never throws. */
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
    // A lost write costs one launch in the old colour, nothing else.
  }
}

/**
 * The accent a loaded preferences blob asks for, or null to keep what is on
 * screen. `get_preferences` answers `{}` both for "never picked one" and for
 * "could not read it" — before the PIN unlock the remote read fails and the
 * local cache is still locked — so a blob without a valid `accent_hex` means
 * the default ONLY when it was read after the unlock (`trusted`). Reading the
 * pre-unlock `{}` as amber would undo the boot accent on every cold start.
 */
export function accentFromPreferences(
  prefs: { accent_hex?: unknown } | null | undefined,
  trusted: boolean,
): string | null {
  if (!prefs) {
    return null;
  }
  return normalizeAccentHex(prefs.accent_hex) ?? (trusted ? DEFAULT_ACCENT_HEX : null);
}

/**
 * The accent to switch to when the signed-in user changes: back to the
 * default on sign-out, so the next account on this device does not open in
 * the last one's colour. A cold start (no user yet → restored user) and a
 * sign-in keep the boot accent; the synced preference takes over from there
 * (an account that never picked one is set back to the default by its first
 * post-unlock read, which also covers a reinstall on iOS, whose keychain
 * outlives the app).
 */
export function accentOnUserChange(
  previousUserId: string | null,
  nextUserId: string | null,
): string | null {
  if (previousUserId && !nextUserId) {
    return DEFAULT_ACCENT_HEX;
  }
  return null;
}
