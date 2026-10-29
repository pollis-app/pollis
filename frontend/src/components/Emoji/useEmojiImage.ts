import { useEffect, useState } from "react";
import { invoke } from "../../bridge";

/**
 * Resolve a custom emoji's content hash to a loopback URL an `<img>` can use.
 *
 * The Rust side fetches the (unencrypted) object, verifies `sha256(bytes)`
 * against the hash and the size ceiling, caches it, and hands back a
 * `http://127.0.0.1:<port>/<token>/<hash>` URL — bytes never cross the JSON IPC.
 *
 * Deliberately unauthorized: this is the render path, and #848 requires a user
 * who is in NONE of the groups that registered an emoji to still see it when
 * someone sends it. The permission rule applies to *sending*, never to viewing.
 *
 * The in-flight map is module-level rather than per-component because a picker
 * renders hundreds of cells at once and a message list re-renders constantly;
 * without it the same hash would be resolved once per mount.
 */
const inFlight = new Map<string, Promise<string>>();

/**
 * The promise for one hash's URL, shared across every caller.
 *
 * Exported because the composer's rich input builds its emoji nodes
 * imperatively (a `contentEditable`'s children cannot be React's, or the caret
 * dies on every reconciliation), so it has no component to hang `useEmojiImage`
 * off — but it must hit the same in-flight cache, or a message full of the same
 * emoji would resolve it once per node.
 */
export function resolveEmojiUrl(contentHash: string): Promise<string> {
  const cached = inFlight.get(contentHash);
  if (cached) {
    return cached;
  }
  // Only the hash: the Rust side looks the content type up from the object row,
  // so an animated emoji cannot be fetched under a static emoji's key.
  const promise = invoke<string>("get_emoji_url", { contentHash });
  inFlight.set(contentHash, promise);
  promise.catch(() => {
    // Drop the rejection so a transient failure (offline, locked) retries on
    // the next render rather than being cached as permanently broken.
    inFlight.delete(contentHash);
  });
  return promise;
}

export interface EmojiImageState {
  url: string | null;
  failed: boolean;
}

/**
 * How long after each failed resolve the next attempt runs (#1562). A failure
 * is not final: one offline moment or a cold DS used to leave the
 * `:shortcode:` text in place until the row remounted. The schedule is short
 * and finite (no polling); past it, the window coming back into view or the
 * network coming back tries again.
 */
export const EMOJI_RETRY_DELAYS_MS = [2_000, 10_000, 30_000] as const;

/** Load one custom emoji's image URL, retrying a failed resolve. */
export function useEmojiImage(contentHash: string): EmojiImageState {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let mounted = true;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let resolving = false;
    let lastFailed = false;
    setUrl(null);
    setFailed(false);

    const resolve = () => {
      resolving = true;
      resolveEmojiUrl(contentHash)
        .then((resolved) => {
          if (mounted) {
            lastFailed = false;
            setUrl(resolved);
            setFailed(false);
          }
        })
        .catch((e: unknown) => {
          if (!mounted) {
            return;
          }
          console.warn(`[emoji] resolving ${contentHash} failed:`, e);
          lastFailed = true;
          setFailed(true);
          const delay = EMOJI_RETRY_DELAYS_MS[attempt];
          attempt += 1;
          if (delay !== undefined) {
            timer = setTimeout(() => {
              timer = null;
              resolve();
            }, delay);
          }
        })
        .finally(() => {
          resolving = false;
        });
    };

    const onBack = () => {
      if (document.visibilityState === "hidden") {
        return;
      }
      if (mounted && lastFailed && timer === null && !resolving) {
        resolve();
      }
    };

    resolve();
    window.addEventListener("online", onBack);
    document.addEventListener("visibilitychange", onBack);
    return () => {
      mounted = false;
      if (timer !== null) {
        clearTimeout(timer);
      }
      window.removeEventListener("online", onBack);
      document.removeEventListener("visibilitychange", onBack);
    };
  }, [contentHash]);

  return { url, failed };
}
