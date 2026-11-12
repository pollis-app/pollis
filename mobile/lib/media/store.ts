// Decrypted-media bookkeeping (#346), pure: lib/media/cache.ts wires it to Rust and the file system.
//
// - One decrypted file per content hash, shared by every consumer and ref-counted.
// - The last release does not unlink at once: the file joins a small most-recently-released set,
//   so a tile that remounts (a Vault tab switch, a row scrolled back into view) finds its path
//   synchronously and draws on its first frame. The set is bounded; the oldest beyond the bound,
//   and every entry on `trim()` (lock, background), is unlinked.
// - `clear()` (sign-out) forgets everything; a resolve or release from the ended session is a no-op.
// - A successful decrypt is announced to `subscribe`rs of that hash (#1826), so a tile that failed
//   earlier recovers the moment any other consumer (the viewer, another row) gets the file.

export interface MediaStoreDeps {
  /** Best-effort unlink of a decrypted file. */
  unlink(uri: string): Promise<void>;
  /** Most unreferenced files kept for a revisit. */
  retainMax: number;
}

export interface MediaStore {
  generation(): number;
  /** The decrypted file for `contentHash` if one is on disk now (held or retained), else null. */
  peek(contentHash: string): string | null;
  /** Take a reference, decrypting (to a sandbox `file://`) if no file is on disk; pair with `release`. */
  acquire(contentHash: string, decrypt: () => Promise<string>): Promise<string>;
  /** Pass the generation captured at acquire: a release from before a `clear()` is a no-op. */
  release(contentHash: string, acquiredIn?: number): Promise<void>;
  /** Unlink every unreferenced retained file (lock, app to the background). */
  trim(): Promise<void>;
  /** Sign-out: forget everything (the caller deletes the directory). */
  clear(): void;
  /** Called after each successful decrypt of `contentHash`; returns the unsubscribe. */
  subscribe(contentHash: string, listener: () => void): () => void;
  /** Hashes kept with no consumer, oldest first. For tests. */
  retained(): string[];
  /** Live `subscribe` listeners across every hash. For tests. */
  listenerCount(): number;
}

export function createMediaStore(deps: MediaStoreDeps): MediaStore {
  let generation = 0;
  const inFlight = new Map<string, Promise<string>>();
  const refCounts = new Map<string, number>();
  const resolved = new Map<string, string>();
  // Insertion order = release order: the first key is the oldest.
  const retained = new Set<string>();
  const listeners = new Map<string, Set<() => void>>();

  function announce(hash: string): void {
    const set = listeners.get(hash);
    if (!set) {
      return;
    }
    // A copy: a listener unsubscribes (and may resubscribe) while we walk.
    for (const listener of [...set]) {
      listener();
    }
  }

  function forget(hash: string): string | undefined {
    const uri = resolved.get(hash);
    inFlight.delete(hash);
    resolved.delete(hash);
    retained.delete(hash);
    return uri;
  }

  async function unlinkAll(uris: Array<string | undefined>): Promise<void> {
    await Promise.all(
      uris.filter((u): u is string => !!u).map((u) => deps.unlink(u).catch(() => undefined)),
    );
  }

  // A resolved file nobody references any more: keep it for a revisit, evicting beyond the bound.
  async function retain(hash: string): Promise<void> {
    retained.delete(hash);
    retained.add(hash);
    const evicted: Array<string | undefined> = [];
    while (retained.size > deps.retainMax) {
      const oldest = retained.values().next().value as string;
      evicted.push(forget(oldest));
    }
    await unlinkAll(evicted);
  }

  return {
    generation: () => generation,

    peek(hash) {
      return resolved.get(hash) ?? null;
    },

    acquire(hash, decrypt) {
      refCounts.set(hash, (refCounts.get(hash) ?? 0) + 1);
      retained.delete(hash);
      const existing = inFlight.get(hash);
      if (existing) {
        return existing;
      }
      const startedIn = generation;
      const promise: Promise<string> = decrypt().then(async (uri) => {
        if (startedIn !== generation) {
          // Cleared (sign-out) mid-decrypt: never hand plaintext from the ended session to anyone.
          await deps.unlink(uri).catch(() => undefined);
          throw new Error(`media cache cleared while resolving ${hash}`);
        }
        if (inFlight.get(hash) !== promise) {
          // Evicted or trimmed while decrypting: nobody may find this file again.
          await deps.unlink(uri).catch(() => undefined);
          return uri;
        }
        resolved.set(hash, uri);
        if (!refCounts.has(hash)) {
          // Every consumer left before the decrypt landed: keep it like any other release.
          await retain(hash);
        }
        announce(hash);
        return uri;
      });
      inFlight.set(hash, promise);
      promise.catch(() => {
        // Let the next caller retry; only drop our own entry.
        if (inFlight.get(hash) === promise) {
          inFlight.delete(hash);
        }
      });
      return promise;
    },

    async release(hash, acquiredIn) {
      if (acquiredIn !== undefined && acquiredIn !== generation) {
        return;
      }
      const count = refCounts.get(hash);
      if (count === undefined) {
        return;
      }
      if (count > 1) {
        refCounts.set(hash, count - 1);
        return;
      }
      refCounts.delete(hash);
      if (resolved.has(hash)) {
        await retain(hash);
      }
      // Still decrypting: `acquire` retains it when it lands.
    },

    async trim() {
      const uris = [...retained].map((hash) => forget(hash));
      await unlinkAll(uris);
    },

    clear() {
      generation += 1;
      inFlight.clear();
      refCounts.clear();
      resolved.clear();
      retained.clear();
    },

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

    retained: () => [...retained],

    listenerCount() {
      let n = 0;
      for (const set of listeners.values()) {
        n += set.size;
      }
      return n;
    },
  };
}
