/**
 * Device-local language storage.
 *
 * Language is per-device, NOT synced through the remote preferences blob —
 * deliberately, and for two reasons:
 *
 *  1. It is an OS-level concern, like the keyboard layout or the font size
 *     (`loadDeviceFontSize` in `colorUtils.ts`, whose scheme this mirrors).
 *     A shared machine in a Spanish office and a personal laptop in English
 *     is a real configuration, and syncing would break it.
 *  2. Decisively: the synced blob needs a signed-in user AND an unlocked
 *     local DB. The login, PIN-entry and enrollment screens have neither, so
 *     a synced-only preference could not localize the very screens a user who
 *     does not read English hits first.
 *
 * Keyed by user id so a shared OS account keeps each Pollis user's choice
 * separate, exactly like font size. Saving for a signed-in user ALSO writes
 * the device-wide entry, so the next pre-auth boot speaks the language the
 * person on this machine last chose rather than reverting to the OS guess.
 */

import { normalizeLanguage } from "./languages";

const LANGUAGE_KEY_PREFIX = "pollis-language:";

/** The pre-auth / device-wide entry, used when no user is signed in yet. */
const DEVICE_SCOPE = "device";

function languageKey(userId: string | null | undefined): string {
  return `${LANGUAGE_KEY_PREFIX}${userId ?? DEVICE_SCOPE}`;
}

function readKey(key: string): string | null {
  try {
    return normalizeLanguage(localStorage.getItem(key));
  } catch {
    // localStorage unavailable (private mode, disabled storage) — treat as unset.
    return null;
  }
}

/**
 * Returns the device-local language for this user, or null if unset.
 *
 * Falls back to the device-wide entry so a user signing in on a machine that
 * was already switched to another language does not get bounced back to the
 * OS locale for one render.
 */
export function loadDeviceLanguage(userId?: string | null): string | null {
  const scoped = readKey(languageKey(userId));
  if (scoped) {
    return scoped;
  }
  if (userId) {
    return readKey(languageKey(null));
  }
  return null;
}

/** Persists the device-local language for this user and for the device. */
export function saveDeviceLanguage(
  userId: string | null | undefined,
  language: string,
): void {
  const normalized = normalizeLanguage(language);
  if (!normalized) {
    return;
  }
  try {
    localStorage.setItem(languageKey(userId), normalized);
    if (userId) {
      localStorage.setItem(languageKey(null), normalized);
    }
  } catch {
    // localStorage unavailable / quota exceeded — fall through silently.
  }
}
