// The share the picker is showing (#2100), held here because a pushed page takes string params
// only. Keyed by the intake's share id. Import-free.

import type { ShareDraft } from "./shareRules";

const drafts = new Map<string, ShareDraft>();

export function holdShare(draft: ShareDraft): void {
  drafts.set(draft.id, draft);
}

export function heldShare(id: string | undefined): ShareDraft | null {
  return (id && drafts.get(id)) || null;
}

export function releaseShare(id: string): void {
  drafts.delete(id);
}

export function releaseAllShares(): void {
  drafts.clear();
}
