// `get_emoji_path` verifies the object hash and writes the image to the cache
// dir as a `file://` URI. Unlike message media, emoji files are public group
// assets verified by hash, tiny and heavily reused, so they stay cached.

import * as FileSystem from "expo-file-system/legacy";
import { invoke } from "./native";

export const EMOJI_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-emoji/`;

// Module-level so every component shares one in-flight promise per hash —
// a burst of the same emoji in one message must not fan out N downloads.
// Failures are not cached: delete the entry so a later mount can retry.
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

/**
 * Drop a resolved hash so the next `resolveEmojiUri` asks the bridge again,
 * which re-downloads and rewrites the file. For an image that failed to draw
 * from the path it was given (the OS may have purged the cache dir).
 */
export function forgetEmojiUri(contentHash: string): void {
  inFlight.delete(contentHash);
}

/** Wipe the emoji cache (sign-out hygiene). */
export async function clearEmojiCache(): Promise<void> {
  inFlight.clear();
  try {
    await FileSystem.deleteAsync(EMOJI_DIR, { idempotent: true });
  } catch {
    // Best-effort.
  }
}
