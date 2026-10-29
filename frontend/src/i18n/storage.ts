/**
 * Device-local language storage, never synced: the login, PIN and enrollment
 * screens have no unlocked DB to read synced prefs from. Keyed by user id; a
 * signed-in save also writes the device-wide entry for the next pre-auth boot.
 * See `.codesight/wiki/i18n.md`.
 */

import { normalizeLanguage } from "./languages";

const LANGUAGE_KEY_PREFIX = "pollis-language:";

const DEVICE_SCOPE = "device";

function languageKey(userId: string | null | undefined): string {
  return `${LANGUAGE_KEY_PREFIX}${userId ?? DEVICE_SCOPE}`;
}

function readKey(key: string): string | null {
  try {
    return normalizeLanguage(localStorage.getItem(key));
  } catch {
      // localStorage unavailable: treat as unset.
    return null;
  }
}

/** This user's language, else the device-wide entry, else null. */
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
    // localStorage unavailable / quota exceeded: ignore.
  }
}
