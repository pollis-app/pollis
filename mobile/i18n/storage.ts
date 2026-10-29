/**
 * Device-local language preference, same contract as desktop's
 * `i18n/storage.ts`: device-scoped with a per-user override, and never the
 * synced preferences blob, which needs a signed-in user and an unlocked local
 * DB — the sign-in, OTP and PIN screens need the language first.
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
