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

/** Load one custom emoji's image URL. */
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
