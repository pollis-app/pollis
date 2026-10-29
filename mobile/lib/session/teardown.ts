// The pure half of ending a session (#1256); wired in ./index.ts.

export interface SessionTeardownDeps {
  resetStore: () => void;
  // Before the caches, so no screen refetches into them.
  leaveSignedInScreens: () => void;
  // It holds decrypted message bodies.
  clearQueryCache: () => Promise<void>;
  // On-disk plaintext; each is wiped independently.
  clearMediaCache: () => Promise<void>;
  clearExportArchives: () => Promise<void>;
  clearEmojiCache: () => Promise<void>;
}

// Every wipe runs even if one fails: a disk hiccup must not strand an ended session.
async function runAll(label: string, wipes: Array<() => Promise<void>>): Promise<void> {
  const results = await Promise.allSettled(wipes.map((wipe) => wipe()));
  for (const result of results) {
    if (result.status === "rejected") {
      console.warn(`[session] ${label} failed (ignored):`, result.reason);
    }
  }
}

// Runs after Rust tore the session down, so nothing resolves fresh data meanwhile.
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

// At most once per JS process: app/index.tsx is re-entered mid-session.
export function oncePerProcess(sweep: () => Promise<void>): () => Promise<void> {
  let pending: Promise<void> | null = null;
  return () => {
    if (!pending) {
      pending = runAll("startup sweep", [sweep]);
    }
    return pending;
  };
}

// Whatever a crashed or killed run left on disk.
export function sweepPlaintext(deps: {
  clearMediaCache: () => Promise<void>;
  clearExportArchives: () => Promise<void>;
}): () => Promise<void> {
  return () => runAll("startup sweep", [deps.clearMediaCache, deps.clearExportArchives]);
}
