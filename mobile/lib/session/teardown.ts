// The pure half of ending a session on this device (#1256). Kept free of
// Expo / MobX / router imports so `node --test` can drive it with fakes; the
// wired versions are `endSession()` and `sweepStalePlaintext()` in ./index.ts.

export interface SessionTeardownDeps {
  // Drop the signed-in UI state (current user, selections, cached lists).
  resetStore: () => void;
  // Unmount every signed-in screen and land on sign-in, so no screen is
  // left mounted to refetch or re-read what is cleared below.
  leaveSignedInScreens: () => void;
  // Cancel in-flight queries, then drop the React Query cache — it holds
  // decrypted message bodies.
  clearQueryCache: () => Promise<void>;
  // On-disk plaintext the session produced. Each is wiped independently.
  clearMediaCache: () => Promise<void>;
  clearExportArchives: () => Promise<void>;
  clearEmojiCache: () => Promise<void>;
}

// Run every wipe even if one fails, logging failures rather than throwing:
// a disk hiccup must not strand the user signed in to a session that has
// already ended, nor keep the other wipes from running.
async function runAll(label: string, wipes: Array<() => Promise<void>>): Promise<void> {
  const results = await Promise.allSettled(wipes.map((wipe) => wipe()));
  for (const result of results) {
    if (result.status === "rejected") {
      console.warn(`[session] ${label} failed (ignored):`, result.reason);
    }
  }
}

// Runs after the Rust side has torn the session down (`logout` /
// `delete_account`), so nothing can resolve fresh data while it is cleared.
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

// Wrap a startup sweep so it runs at most once per JS process, however
// often it is asked for. The boot screen (`app/index.tsx`) is re-entered
// mid-session (the invite screen routes back to `/`), so a sweep tied to it
// could wipe media out from under mounted screens; the root layout calls
// this before anything restores a session, and only the first call sweeps.
export function oncePerProcess(sweep: () => Promise<void>): () => Promise<void> {
  let pending: Promise<void> | null = null;
  return () => {
    if (!pending) {
      pending = runAll("startup sweep", [sweep]);
    }
    return pending;
  };
}

// The startup sweep proper: whatever a crashed or killed run left on disk.
// Nothing has resolved media yet at that point, so nothing is mounted on it.
export function sweepPlaintext(deps: {
  clearMediaCache: () => Promise<void>;
  clearExportArchives: () => Promise<void>;
}): () => Promise<void> {
  return () => runAll("startup sweep", [deps.clearMediaCache, deps.clearExportArchives]);
}
