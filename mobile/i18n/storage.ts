/** Device-local language preference, per user, never the synced blob: the pre-auth screens need it. */

import * as SecureStore from "expo-secure-store";
import { languageKey, normalizeLanguage } from "./languages";

async function read(key: string): Promise<string | null> {
  try {
    return normalizeLanguage(await SecureStore.getItemAsync(key));
  } catch {
    return null;
  }
}

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
    // A lost preference costs one re-pick.
  }
}
