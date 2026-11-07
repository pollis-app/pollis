// In-memory composer drafts by room; never persisted, synchronous, cleared when the active user changes or the session ends.
// mobile/lib/drafts.ts is a byte-for-byte copy (mobile/tests/drafts.test.ts).

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
    drafts.delete(key);
    return;
  }
  drafts.set(key, value);
}

export function clearAllDrafts(): void {
  drafts.clear();
}
