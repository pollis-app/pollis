// Custom-emoji image resolution. `get_emoji_path` (bridge PR #967) verifies
// the object hash and materialises the image to the sandbox cache dir,
// returning a `file://` URI — the emoji analogue of `get_media_path`
// (mobile can't run desktop's loopback media server).
//
// Unlike message media (plaintext unlinked on last release), emoji files are
// tiny, unencrypted-at-rest by design (they're public group assets verified
// by hash) and heavily reused across messages, so they stay cached.

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

/** Wipe the emoji cache (sign-out hygiene). */
export async function clearEmojiCache(): Promise<void> {
  inFlight.clear();
  try {
    await FileSystem.deleteAsync(EMOJI_DIR, { idempotent: true });
  } catch {
    // Best-effort.
  }
}
