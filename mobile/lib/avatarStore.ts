// Which picture to draw for a user, and when to fetch one (#avatars). Pure: lib/avatarCache.ts wires it.
//
// - Files on disk are the source of truth for "what can be drawn right now". One directory
//   listing at launch maps every user to the picture last drawn for them, so a revisit
//   draws the photo on its first frame — before any profile read, with no letter flash.
// - A user's avatar KEY comes from whatever list already carries it (DM roster, members,
//   profile, search) or, failing that, one batched profile read for every unknown user.
// - A changed key keeps the old picture on screen until the new one is on disk; a removed
//   one drops the files at once.

import { avatarHash, parseRenditionFile, siblingUri, AVATAR_RENDITIONS } from "./avatarKey.ts";

export interface AvatarStoreDeps {
  /** File names in the avatar directory ([] when it does not exist). */
  listFiles(): Promise<string[]>;
  /** `file://` URI of a name in the avatar directory. */
  fileUri(name: string): string;
  /** Core: a rendition of `key` on disk, fetched if needed. */
  resolve(userId: string, key: string, px: number): Promise<string>;
  /** Core: avatar keys for these users (one round trip). Unknown users are left out. */
  fetchKeys(userIds: string[]): Promise<Array<{ id: string; avatar_url?: string | null }>>;
  /** Best-effort delete of local files. */
  deleteFiles(uris: string[]): void;
  /** Defers the profile batch to the end of the current burst of renders. */
  schedule(fn: () => void): void;
  /** Warms the image decoder's memory cache. */
  warm(uris: string[]): void;
}

// Most ids sent in one profile read.
export const PROFILE_BATCH_MAX = 100;

interface Picture {
  hash: string;
  // px → uri
  uris: Map<number, string>;
}

export interface AvatarStore {
  hydrate(): Promise<void>;
  /** The URI to draw now for `userId` at `px`, or null for the letter avatar. */
  view(userId: string, px: number): string | null;
  /** Record a key a list carried. `undefined` = this list does not know. */
  noteKey(userId: string, key: string | null | undefined): void;
  /** Make `view` able to answer: fetch the key and/or the file if missing. */
  ensure(userId: string, px: number): void;
  /** `ensure` for a list's rows, then warm the decoder for what is on disk. */
  prefetch(rows: Array<{ userId: string; key?: string | null }>, px: number): void;
  /** Our own new picture, already written by `upload_avatar`. */
  adopt(userId: string, key: string, uri: string | null): void;
  /** An image that would not draw (purged by the OS): forget it and fetch again. */
  forget(userId: string, uri: string): void;
  /** Let failed fetches try again (the app came back to the foreground). */
  retryFailed(): void;
  clear(): void;
  subscribe(listener: () => void): () => void;
}

export function createAvatarStore(deps: AvatarStoreDeps): AvatarStore {
  const pictures = new Map<string, Picture>();
  // userId → key; null = known to have none.
  const keys = new Map<string, string | null>();
  const keyRequested = new Set<string>();
  const queued = new Set<string>();
  const inFlight = new Set<string>();
  const failed = new Set<string>();
  const listeners = new Set<() => void>();
  // URIs that failed to draw once; a second failure stops refetching.
  const drawFailures = new Set<string>();
  // Inside prefetch: one emit for the whole list.
  let batching = 0;
  let pendingEmit = false;
  let hydrated: Promise<void> | null = null;
  let flushScheduled = false;
  // Bumped by clear(), so a fetch that lands after sign-out writes nothing.
  let epoch = 0;

  const emit = () => {
    if (batching > 0) {
      pendingEmit = true;
      return;
    }
    for (const l of [...listeners]) {
      l();
    }
  };

  const dropPicture = (userId: string) => {
    const p = pictures.get(userId);
    if (!p) {
      return;
    }
    pictures.delete(userId);
    deps.deleteFiles([...p.uris.values()]);
  };

  const setPicture = (userId: string, hash: string, uri: string) => {
    const current = pictures.get(userId);
    if (current && current.hash !== hash) {
      // Core already removed the old files; only the map is stale.
      pictures.delete(userId);
    }
    // Every rendition is written together, so name them all.
    const uris = new Map<number, string>();
    for (const px of AVATAR_RENDITIONS) {
      uris.set(px, siblingUri(uri, px));
    }
    pictures.set(userId, { hash, uris });
  };

  const fetchFile = (userId: string, key: string, hash: string, px: number) => {
    const id = `${userId}|${hash}|${px}`;
    if (inFlight.has(id) || failed.has(id)) {
      return;
    }
    inFlight.add(id);
    const at = epoch;
    deps
      .resolve(userId, key, px)
      .then((uri) => {
        if (at !== epoch || keys.get(userId) !== key) {
          return;
        }
        setPicture(userId, hash, uri);
        deps.warm([uri]);
        emit();
      })
      .catch(() => {
        if (at === epoch) {
          failed.add(id);
        }
      })
      .finally(() => {
        inFlight.delete(id);
      });
  };

  const flush = () => {
    flushScheduled = false;
    const ids = [...queued];
    queued.clear();
    const at = epoch;
    for (let i = 0; i < ids.length; i += PROFILE_BATCH_MAX) {
      const chunk = ids.slice(i, i + PROFILE_BATCH_MAX);
      deps
        .fetchKeys(chunk)
        .then((profiles) => {
          if (at !== epoch) {
            return;
          }
          const byId = new Map(profiles.map((p) => [p.id, p.avatar_url ?? null]));
          for (const id of chunk) {
            // A list may have answered meanwhile; it is at least as fresh.
            if (!keys.has(id)) {
              store.noteKey(id, byId.get(id) ?? null);
            }
          }
        })
        .catch(() => {
          // Lazy recovery: the next render asks again.
          for (const id of chunk) {
            keyRequested.delete(id);
          }
        });
    }
  };

  const store: AvatarStore = {
    hydrate() {
      if (!hydrated) {
        const at = epoch;
        hydrated = deps
          .listFiles()
          .then((names) => {
            if (at !== epoch) {
              return;
            }
            for (const name of names) {
              const f = parseRenditionFile(name);
              if (!f) {
                continue;
              }
              const p = pictures.get(f.userId);
              if (p && p.hash !== f.hash) {
                // Two pictures for one user (a crash mid-swap): unknown which is newer.
                continue;
              }
              const entry = p ?? { hash: f.hash, uris: new Map<number, string>() };
              entry.uris.set(f.px, deps.fileUri(name));
              pictures.set(f.userId, entry);
            }
            emit();
          })
          .catch(() => {});
      }
      return hydrated;
    },

    view(userId, px) {
      const key = keys.get(userId);
      if (key === null) {
        return null;
      }
      const p = pictures.get(userId);
      if (!p) {
        return null;
      }
      return p.uris.get(px) ?? [...p.uris.values()][0] ?? null;
    },

    noteKey(userId, key) {
      if (key === undefined) {
        return;
      }
      const hash = avatarHash(key);
      const next = hash ? key : null;
      const prev = keys.get(userId);
      keys.set(userId, next);
      if (prev === next) {
        return;
      }
      if (next === null) {
        if (pictures.has(userId)) {
          dropPicture(userId);
        }
        emit();
        return;
      }
      emit();
    },

    ensure(userId, px) {
      const key = keys.get(userId);
      if (key === undefined) {
        if (!keyRequested.has(userId)) {
          keyRequested.add(userId);
          queued.add(userId);
          if (!flushScheduled) {
            flushScheduled = true;
            deps.schedule(flush);
          }
        }
        return;
      }
      if (key === null) {
        return;
      }
      const hash = avatarHash(key);
      if (!hash) {
        return;
      }
      const p = pictures.get(userId);
      if (p && p.hash === hash && p.uris.has(px)) {
        return;
      }
      fetchFile(userId, key, hash, px);
    },

    prefetch(rows, px) {
      batching += 1;
      try {
        for (const row of rows) {
          store.noteKey(row.userId, row.key);
          store.ensure(row.userId, px);
        }
      } finally {
        batching -= 1;
      }
      if (batching === 0 && pendingEmit) {
        pendingEmit = false;
        emit();
      }
      const uris: string[] = [];
      for (const row of rows) {
        const uri = store.view(row.userId, px);
        if (uri) {
          uris.push(uri);
        }
      }
      if (uris.length > 0) {
        deps.warm(uris);
      }
    },

    adopt(userId, key, uri) {
      const hash = avatarHash(key);
      keys.set(userId, hash ? key : null);
      if (hash && uri) {
        setPicture(userId, hash, uri);
      }
      emit();
    },

    forget(userId, uri) {
      const p = pictures.get(userId);
      if (!p) {
        return;
      }
      const again = drawFailures.has(uri);
      drawFailures.add(uri);
      for (const [px, u] of p.uris) {
        if (u === uri) {
          p.uris.delete(px);
          if (again) {
            // Downloaded afresh and still undrawable: stop until retryFailed().
            failed.add(`${userId}|${p.hash}|${px}`);
          }
        }
      }
      if (p.uris.size === 0) {
        pictures.delete(userId);
      }
      // Core returns a file that exists without reading it, so delete it to refetch.
      deps.deleteFiles([uri]);
      emit();
    },

    retryFailed() {
      failed.clear();
      drawFailures.clear();
      emit();
    },

    clear() {
      epoch += 1;
      pictures.clear();
      keys.clear();
      keyRequested.clear();
      queued.clear();
      inFlight.clear();
      failed.clear();
      drawFailures.clear();
      hydrated = null;
      emit();
    },

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
  return store;
}
