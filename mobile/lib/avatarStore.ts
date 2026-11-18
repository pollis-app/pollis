// Which picture to draw for a user, and when to fetch one (#avatars). Pure: lib/avatarCache.ts wires it.
//
// - Pictures are keyed by their content hash, not by user, so one user's main picture and
//   their per-group alias pictures (#2003) are drawn side by side and never evict each other.
// - Files on disk are the source of truth for "what can be drawn right now". One directory
//   listing at launch maps every user to the picture last drawn for them, so a revisit
//   draws the photo on its first frame — before any profile read, with no letter flash.
// - A user's main avatar KEY comes from whatever list already carries it (DM roster, members,
//   profile, search) or, failing that, one batched profile read for every unknown user.
//   An alias key is passed by the group surface that draws it and never becomes the main key.
// - A changed main key keeps the old picture on screen until the new one is on disk, then
//   deletes the old files; a removed one drops them at once. A hash drawn as an alias this
//   session is never deleted that way.

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
  // px → uri
  uris: Map<number, string>;
}

export interface AvatarStore {
  hydrate(): Promise<void>;
  /**
   * The URI to draw now for `userId` at `px`, or null for the letter avatar. `aliasKey`:
   * the user's alias picture in the group being drawn (#2003); omitted for the main one.
   */
  view(userId: string, px: number, aliasKey?: string | null): string | null;
  /** Record a main key a list carried. `undefined` = this list does not know. */
  noteKey(userId: string, key: string | null | undefined): void;
  /** Make `view` able to answer: fetch the key and/or the file if missing. */
  ensure(userId: string, px: number, aliasKey?: string | null): void;
  /** `ensure` for a list's rows, then warm the decoder for what is on disk. */
  prefetch(rows: Array<{ userId: string; key?: string | null; aliasKey?: string | null }>, px: number): void;
  /** Our own new picture, already written by `upload_avatar` / `upload_avatar_image`. */
  adopt(userId: string, key: string, uri: string | null, opts?: { alias?: boolean }): void;
  /** An image that would not draw (purged by the OS): forget it and fetch again. */
  forget(userId: string, uri: string): void;
  /** Let failed fetches try again (the app came back to the foreground). */
  retryFailed(): void;
  clear(): void;
  subscribe(listener: () => void): () => void;
}

export function createAvatarStore(deps: AvatarStoreDeps): AvatarStore {
  // hash → its renditions on disk.
  const pictures = new Map<string, Picture>();
  // userId → main key; null = known to have none.
  const keys = new Map<string, string | null>();
  // userId → hash of the main picture last drawn (from disk at launch, or a fetch that landed).
  const shown = new Map<string, string>();
  // Hashes drawn as an alias this session: a main-picture change never deletes them.
  const aliasHashes = new Set<string>();
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

  const uriOf = (hash: string | null | undefined, px: number): string | null => {
    if (!hash) {
      return null;
    }
    const p = pictures.get(hash);
    if (!p) {
      return null;
    }
    return p.uris.get(px) ?? [...p.uris.values()][0] ?? null;
  };

  // A main picture nobody draws any more: its files go, unless it is also an alias picture.
  const dropPicture = (hash: string) => {
    if (aliasHashes.has(hash)) {
      return;
    }
    const p = pictures.get(hash);
    if (!p) {
      return;
    }
    pictures.delete(hash);
    deps.deleteFiles([...p.uris.values()]);
  };

  const setPicture = (hash: string, uri: string) => {
    // Every rendition is written together, so name them all.
    const uris = new Map<number, string>();
    for (const px of AVATAR_RENDITIONS) {
      uris.set(px, siblingUri(uri, px));
    }
    pictures.set(hash, { uris });
  };

  // The main picture now drawn for `userId` is `hash`; the one before it goes.
  const showMain = (userId: string, hash: string) => {
    const before = shown.get(userId);
    shown.set(userId, hash);
    if (before && before !== hash) {
      dropPicture(before);
    }
  };

  const fetchFile = (userId: string, key: string, hash: string, px: number, alias: boolean) => {
    const id = `${hash}|${px}`;
    if (inFlight.has(id) || failed.has(id)) {
      return;
    }
    inFlight.add(id);
    const at = epoch;
    deps
      .resolve(userId, key, px)
      .then((uri) => {
        if (at !== epoch) {
          return;
        }
        if (!alias && keys.get(userId) !== key) {
          return;
        }
        setPicture(hash, uri);
        if (!alias) {
          showMain(userId, hash);
        }
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
            const byUser = new Map<string, Set<string>>();
            for (const name of names) {
              const f = parseRenditionFile(name);
              if (!f) {
                continue;
              }
              const entry = pictures.get(f.hash) ?? { uris: new Map<number, string>() };
              entry.uris.set(f.px, deps.fileUri(name));
              pictures.set(f.hash, entry);
              const hashes = byUser.get(f.userId) ?? new Set<string>();
              hashes.add(f.hash);
              byUser.set(f.userId, hashes);
            }
            // One picture on disk is the one last drawn. Several (a main picture and alias
            // pictures, #2003): unknown which is the main one until its key arrives.
            for (const [userId, hashes] of byUser) {
              if (hashes.size === 1 && !shown.has(userId)) {
                shown.set(userId, [...hashes][0]);
              }
            }
            emit();
          })
          .catch(() => {});
      }
      return hydrated;
    },

    view(userId, px, aliasKey) {
      const aliasHash = avatarHash(aliasKey);
      if (aliasHash) {
        return uriOf(aliasHash, px);
      }
      const key = keys.get(userId);
      if (key === null) {
        return null;
      }
      const want = key === undefined ? shown.get(userId) : avatarHash(key);
      // A changed key that is not on disk yet keeps the last drawn picture up.
      return uriOf(want, px) ?? uriOf(shown.get(userId), px);
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
        const before = shown.get(userId);
        shown.delete(userId);
        if (before) {
          dropPicture(before);
        }
      }
      emit();
    },

    ensure(userId, px, aliasKey) {
      const aliasHash = avatarHash(aliasKey);
      if (aliasHash && aliasKey) {
        aliasHashes.add(aliasHash);
        if (!pictures.get(aliasHash)?.uris.has(px)) {
          fetchFile(userId, aliasKey, aliasHash, px, true);
        }
        return;
      }
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
      if (pictures.get(hash)?.uris.has(px)) {
        shown.set(userId, hash);
        return;
      }
      fetchFile(userId, key, hash, px, false);
    },

    prefetch(rows, px) {
      batching += 1;
      try {
        for (const row of rows) {
          store.noteKey(row.userId, row.key);
          store.ensure(row.userId, px, row.aliasKey);
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
        const uri = store.view(row.userId, px, row.aliasKey);
        if (uri) {
          uris.push(uri);
        }
      }
      if (uris.length > 0) {
        deps.warm(uris);
      }
    },

    adopt(userId, key, uri, opts) {
      const hash = avatarHash(key);
      if (opts?.alias) {
        if (hash) {
          aliasHashes.add(hash);
        }
      } else {
        keys.set(userId, hash ? key : null);
      }
      if (hash && uri) {
        setPicture(hash, uri);
        if (!opts?.alias) {
          showMain(userId, hash);
        }
      }
      emit();
    },

    forget(_userId, uri) {
      for (const [hash, p] of pictures) {
        let hit = false;
        for (const [px, u] of p.uris) {
          if (u === uri) {
            hit = true;
            p.uris.delete(px);
            if (drawFailures.has(uri)) {
              // Downloaded afresh and still undrawable: stop until retryFailed().
              failed.add(`${hash}|${px}`);
            }
          }
        }
        if (hit && p.uris.size === 0) {
          pictures.delete(hash);
        }
      }
      drawFailures.add(uri);
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
      shown.clear();
      aliasHashes.clear();
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
