// Custom-emoji images via `get_emoji_path`; public, hash-verified and reused, so unlike media they stay cached.

import * as FileSystem from "expo-file-system/legacy";
import { invoke } from "./native";

export const EMOJI_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-emoji/`;

// One in-flight promise per hash; failures are not cached.
const inFlight = new Map<string, Promise<string>>();

export function resolveEmojiUri(contentHash: string): Promise<string> {
  const existing = inFlight.get(contentHash);
  if (existing) {
    return existing;
  }
  const promise = invoke<string>("get_emoji_path", {
    contentHash,
    destDir: EMOJI_DIR,
  }).then((uri) => {
    if (!uri) {
      throw new Error(`get_emoji_path returned empty path for ${contentHash}`);
    }
    return uri;
  });
  promise.catch(() => {
    inFlight.delete(contentHash);
  });
  inFlight.set(contentHash, promise);
  return promise;
}

/** For an image that failed to draw (the OS may have purged the cache dir). */
export function forgetEmojiUri(contentHash: string): void {
  inFlight.delete(contentHash);
}

export async function clearEmojiCache(): Promise<void> {
  inFlight.clear();
  try {
    await FileSystem.deleteAsync(EMOJI_DIR, { idempotent: true });
  } catch {
    // Best-effort.
  }
}
