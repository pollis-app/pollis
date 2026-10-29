/** Device-local language per user, not synced: pre-auth screens have no unlocked prefs. Also writes a device-wide entry. */

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
  }
}
