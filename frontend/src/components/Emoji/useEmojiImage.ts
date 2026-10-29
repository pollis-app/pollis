import { useEffect, useState } from "react";
import { invoke } from "../../bridge";

/** Content hash → loopback URL (Rust verifies the hash). Ungated (#848). Module-level in-flight map dedupes across mounts. */
const inFlight = new Map<string, Promise<string>>();

/** Shared promise per hash; exported for the rich input's imperative emoji nodes. */
export function resolveEmojiUrl(contentHash: string): Promise<string> {
  const cached = inFlight.get(contentHash);
  if (cached) {
    return cached;
  }
  // Only the hash: Rust looks up the content type itself.
  const promise = invoke<string>("get_emoji_url", { contentHash });
  inFlight.set(contentHash, promise);
  promise.catch(() => {
    // Drop the rejection so a transient failure retries.
    inFlight.delete(contentHash);
  });
  return promise;
}

export interface EmojiImageState {
  url: string | null;
  failed: boolean;
}

/** Retry delays after a failed resolve (#1562); finite, no polling. */
export const EMOJI_RETRY_DELAYS_MS = [2_000, 10_000, 30_000] as const;

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
