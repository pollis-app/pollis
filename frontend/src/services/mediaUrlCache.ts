// Attachment → media URL bookkeeping, pure: services/r2-upload.ts wires it to `get_media_url`.
//
// - One fetch per content hash in flight; resolved URLs stay valid for the document and are
//   readable synchronously, so a remount (Vault Chat ↔ Media) draws at once.
// - A failed fetch is forgotten, so the next caller (a remount, a re-render) fetches again.
// - Every success is announced to that hash's subscribers (#1826): a row or spotlight showing
//   "failed to load" picks the URL up the moment anyone else gets it. Event-driven, no timer.

type Listener = (url: string) => void;

export interface MediaUrlCache {
  /** The URL already resolved for this hash, or null; never starts a fetch. */
  peek(contentHash: string): string | null;
  /** Resolve through `fetch`, sharing one in-flight request per hash. */
  get(contentHash: string, fetch: () => Promise<string>): Promise<string>;
  /** Tell subscribers `url` works for `contentHash` (e.g. an image that drew from it). */
  announce(contentHash: string, url: string): void;
  /** Called with the URL on each later success for `contentHash`; returns the unsubscribe. */
  subscribe(contentHash: string, listener: Listener): () => void;
  /** Live listeners across every hash. For tests. */
  listenerCount(): number;
}

export function createMediaUrlCache(): MediaUrlCache {
  const inFlight = new Map<string, Promise<string>>();
  const resolvedUrls = new Map<string, string>();
  const listeners = new Map<string, Set<Listener>>();

  function announce(hash: string, url: string): void {
    const set = listeners.get(hash);
    if (!set) {
      return;
    }
    // A copy: a listener may unsubscribe while we walk.
    for (const listener of [...set]) {
      listener(url);
    }
  }

  return {
    peek(hash) {
      return resolvedUrls.get(hash) ?? null;
    },

    get(hash, fetch) {
      const cached = inFlight.get(hash);
      if (cached) {
        return cached;
      }
      const promise = fetch();
      inFlight.set(hash, promise);
      promise.then(
        (url) => {
          if (inFlight.get(hash) === promise) {
            resolvedUrls.set(hash, url);
            announce(hash, url);
          }
        },
        () => {
          // Never sticky: the next caller fetches again.
          if (inFlight.get(hash) === promise) {
            inFlight.delete(hash);
          }
        },
      );
      return promise;
    },

    announce,

    subscribe(hash, listener) {
      let set = listeners.get(hash);
      if (!set) {
        set = new Set();
        listeners.set(hash, set);
      }
      set.add(listener);
      return () => {
        const current = listeners.get(hash);
        if (!current) {
          return;
        }
        current.delete(listener);
        if (current.size === 0) {
          listeners.delete(hash);
        }
      };
    },

    listenerCount() {
      let n = 0;
      for (const set of listeners.values()) {
        n += set.size;
      }
      return n;
    },
  };
}
