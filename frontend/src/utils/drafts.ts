// In-memory composer drafts by room (kind-prefixed id). Deliberately NOT
// persisted. Synchronous, so the composer saves in the same call stack as its
// setState. Cleared on every change of active user id (App.tsx), so the next
// user on the machine never sees them.

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
      // Drop empty entries so dead keys don't accumulate.
    drafts.delete(key);
    return;
  }
  drafts.set(key, value);
}

export function clearAllDrafts(): void {
  drafts.clear();
}
