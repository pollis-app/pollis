// In-memory message drafts.
//
// A draft is the unsent text currently sitting in the composer for a given
// room (a group channel id or a DM conversation id, prefixed with a "kind"
// token to avoid the theoretical case of an id colliding across the two
// tables). Drafts are NOT persisted: the Map lives in module scope, so it
// dies on app close and on full page reload. There is no localStorage,
// no IndexedDB, no Rust round-trip — by design.
//
// Lookup and mutation are both O(1) and synchronous, so the composer can
// save the latest value in the same call stack as its own setState on
// every keystroke. No effect, no async, no microtask gap where a tab
// switch could lose the value.
//
// Drafts are cleared on every transition of the active user id (login,
// logout, account switch). That covers the "user A logs out, user B logs
// in on the same machine" case explicitly — user B never sees user A's
// drafts in a channel they both happen to be members of. App.tsx wires
// the `clearAllDrafts()` call to its `currentUser?.id` effect.

const drafts = new Map<string, string>();

export function getDraft(key: string | null | undefined): string {
  if (!key) {
    return "";
  }
  return drafts.get(key) ?? "";
}

export function setDraft(key: string | null | undefined, value: string): void {
  if (!key) {
    return;
  }
  if (value.length === 0) {
    // Drop empty entries so the Map doesn't accumulate dead keys for every
    // channel the user has ever opened.
    drafts.delete(key);
    return;
  }
  drafts.set(key, value);
}

export function clearAllDrafts(): void {
  drafts.clear();
}
