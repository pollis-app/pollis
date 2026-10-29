import { useEffect, useState } from "react";
import { invoke } from "../../bridge";

/**
 * Resolve a custom emoji's content hash to a loopback URL. Rust fetches the
 * unencrypted object, verifies `sha256` and size, caches it, and returns
 * `http://127.0.0.1:<port>/<token>/<hash>`.
 *
 * Deliberately unauthorized: #848 lets anyone view an emoji; the rule gates
 * sending. The in-flight map is module-level so hundreds of cells share it.
 */
const inFlight = new Map<string, Promise<string>>();

/**
 * The shared promise for one hash's URL. Exported for the composer's rich
 * input, which builds emoji nodes imperatively and must hit the same cache.
 */
export function resolveEmojiUrl(contentHash: string): Promise<string> {
  const cached = inFlight.get(contentHash);
  if (cached) {
    return cached;
  }
  // Only the hash: Rust takes the content type from the object row, so an
  // animated emoji can't be fetched under a static one's key.
  const promise = invoke<string>("get_emoji_url", { contentHash });
  inFlight.set(contentHash, promise);
  promise.catch(() => {
    // Not cached as broken, so a transient failure retries on the next render.
    inFlight.delete(contentHash);
  });
  return promise;
}

export interface EmojiImageState {
  url: string | null;
  failed: boolean;
}

export function useEmojiImage(contentHash: string): EmojiImageState {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let mounted = true;
    setUrl(null);
    setFailed(false);

    resolveEmojiUrl(contentHash)
      .then((resolved) => {
        if (mounted) {
          setUrl(resolved);
        }
      })
      .catch(() => {
        if (mounted) {
          setFailed(true);
        }
      });

    return () => {
      mounted = false;
    };
  }, [contentHash]);

  return { url, failed };
}
