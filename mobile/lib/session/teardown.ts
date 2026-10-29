// The pure half of ending a session on this device (#1256), free of Expo / MobX
// / router imports for the node tests; wired in ./index.ts.

export interface SessionTeardownDeps {
  resetStore: () => void;
  // Unmount every signed-in screen, so nothing is left to refetch what is
  // cleared below.
  leaveSignedInScreens: () => void;
  // Cancel in-flight queries, then drop the cache: it holds decrypted bodies.
  clearQueryCache: () => Promise<void>;
  // On-disk plaintext the session produced.
  clearMediaCache: () => Promise<void>;
  clearExportArchives: () => Promise<void>;
  clearEmojiCache: () => Promise<void>;
}

// Every wipe runs even if one fails: a disk hiccup must not strand the user in
// an ended session or skip the other wipes.
async function runAll(label: string, wipes: Array<() => Promise<void>>): Promise<void> {
  const results = await Promise.allSettled(wipes.map((wipe) => wipe()));
  for (const result of results) {
    if (result.status === "rejected") {
      console.warn(`[session] ${label} failed (ignored):`, result.reason);
    }
  }
}

// Runs after Rust tore the session down (`logout` / `delete_account`), so
// nothing can resolve fresh data while it is cleared.
export async function tearDownSession(deps: SessionTeardownDeps): Promise<void> {
  deps.resetStore();
  deps.leaveSignedInScreens();
  await runAll("session teardown", [
    deps.clearQueryCache,
    deps.clearMediaCache,
    deps.clearExportArchives,
    deps.clearEmojiCache,
  ]);
}

// At most once per JS process: `app/index.tsx` is re-entered mid-session, so a
// sweep tied to it could wipe media from under mounted screens.
export function oncePerProcess(sweep: () => Promise<void>): () => Promise<void> {
  let pending: Promise<void> | null = null;
  return () => {
    if (!pending) {
      pending = runAll("startup sweep", [sweep]);
    }
    return pending;
  };
}

// Whatever a crashed or killed run left on disk; nothing is mounted on it yet.
export function sweepPlaintext(deps: {
  clearMediaCache: () => Promise<void>;
  clearExportArchives: () => Promise<void>;
}): () => Promise<void> {
  return () => runAll("startup sweep", [deps.clearMediaCache, deps.clearExportArchives]);
}
