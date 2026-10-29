// In-memory composer drafts by room; never persisted, synchronous, cleared on any active-user change (App.tsx).

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
