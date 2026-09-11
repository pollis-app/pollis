/**
 * Device-local language preference.
 *
 * Same contract as desktop's `i18n/storage.ts` — device-scoped, with a
 * per-user override so two Pollis users on one device keep separate choices,
 * and never the synced preferences blob: that needs a signed-in user and an
 * unlocked local DB, and the sign-in, OTP and PIN screens are the first thing
 * a user who does not read English sees.
 *
 * Stored in `expo-secure-store`, which is already how the auto-lock timeout
 * persists (`lib/autolock.tsx`). Its keys may not contain `:`, so the desktop
 * key shape is spelled with dots here.
 */

import * as SecureStore from "expo-secure-store";
import { languageKey, normalizeLanguage } from "./languages";

async function read(key: string): Promise<string | null> {
  try {
    return normalizeLanguage(await SecureStore.getItemAsync(key));
  } catch {
    return null;
  }
}

/** The stored choice for `userId`, falling back to the device-wide one. */
export async function loadDeviceLanguage(userId?: string | null): Promise<string | null> {
  const scoped = await read(languageKey(userId));
  if (scoped) {
    return scoped;
  }
  if (userId) {
    return read(languageKey(null));
  }
  return null;
}

export async function saveDeviceLanguage(
  userId: string | null | undefined,
  language: string,
): Promise<void> {
  const normalized = normalizeLanguage(language);
  if (!normalized) {
    return;
  }
  try {
    await SecureStore.setItemAsync(languageKey(userId), normalized);
    if (userId) {
      await SecureStore.setItemAsync(languageKey(null), normalized);
    }
  } catch {
    // A lost preference costs the user one re-pick and nothing else.
  }
}
