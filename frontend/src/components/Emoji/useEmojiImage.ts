import { useEffect, useState } from "react";
import { invoke } from "../../bridge";

/**
 * Resolve a custom emoji's content hash to a verified loopback URL. Deliberately
 * unauthorized: anyone who receives an emoji may view it (#848). Module-level so
 * every mount shares one in-flight resolve per hash.
 */
const inFlight = new Map<string, Promise<string>>();

/**
 * The promise for one hash's URL, shared across every caller. Exported for the
 * composer's imperatively built `contentEditable` nodes, which have no component
 * to hang `useEmojiImage` off.
 */
export function resolveEmojiUrl(contentHash: string): Promise<string> {
  const cached = inFlight.get(contentHash);
  if (cached) {
    return cached;
  }
  // Only the hash: Rust looks the content type up itself, so an animated emoji
  // cannot be fetched under a static emoji's key.
  const promise = invoke<string>("get_emoji_url", { contentHash });
  inFlight.set(contentHash, promise);
  promise.catch(() => {
    // Uncache the rejection so a transient failure retries on the next render.
    inFlight.delete(contentHash);
  });
  return promise;
}

export interface EmojiImageState {
  url: string | null;
  failed: boolean;
}

/**
 * Delay before each retry of a failed resolve (#1562). Finite, so not polling;
 * past it, regaining visibility or the network retries.
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
