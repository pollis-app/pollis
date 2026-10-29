// Custom-emoji images via `get_emoji_path`, which verifies the hash and writes
// a `file://` in the sandbox cache. Unlike message media these stay cached:
// public group assets, verified by hash, tiny and heavily reused.

import * as FileSystem from "expo-file-system/legacy";
import { invoke } from "./native";

export const EMOJI_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-emoji/`;

// One in-flight promise per hash, so a burst of one emoji doesn't fan out N
// downloads. Failures are not cached, so a later mount can retry.
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

export async function clearEmojiCache(): Promise<void> {
  inFlight.clear();
  try {
    await FileSystem.deleteAsync(EMOJI_DIR, { idempotent: true });
  } catch {
    // Best-effort.
  }
}
