// Pure rules behind the Vault screen (#1316), ports of desktop's Vault.tsx.

import type { MessageAttachment } from "../types";
import { parseContent, type AttachmentWire } from "./messageContent.ts";
import { isRollMedia, mediaKind, type ViewerItems } from "./media/viewer.ts";

export interface VaultMessage {
  id: string;
  /** Plain text or the `_att`/`_txt` envelope; parse with `parseContent`. */
  content: string;
  pinned: boolean;
  created_at: string;
  updated_at: string;
}

/** Pin/unpin counts as a rewrite too (the flag lives inside the ciphertext). */
export function isVaultEntryEdited(entry: VaultMessage): boolean {
  return entry.updated_at !== entry.created_at;
}

/** Client-side search: the server cannot search what it cannot read. */
export function filterVaultEntries(
  entries: readonly VaultMessage[],
  query: string,
): VaultMessage[] {
  const needle = query.trim().toLowerCase();
  if (!needle) {
    return [...entries];
  }
  return entries.filter((entry) => {
    const parsed = parseContent(entry.content, "vault");
    const haystacks = [parsed.text, ...parsed.attachments.map((a) => a.filename)];
    return haystacks.some((h) => h.toLowerCase().includes(needle));
  });
}

/** Every attachment, newest entry first, each once. */
export function vaultAttachments(entries: readonly VaultMessage[]): MessageAttachment[] {
  const out: MessageAttachment[] = [];
  const seen = new Set<string>();
  for (let i = entries.length - 1; i >= 0; i--) {
    for (const attachment of parseContent(entries[i].content, "vault").attachments) {
      if (!seen.has(attachment.id)) {
        seen.add(attachment.id);
        out.push(attachment);
      }
    }
  }
  return out;
}

/** An entry with files keeps them across a text edit; null when nothing to save. */
export function editedVaultContent(original: string, draft: string): string | null {
  const text = draft.trim();
  const attachments = parseContent(original, "vault").attachments;
  if (attachments.length === 0) {
    return text || null;
  }
  const att: AttachmentWire[] = attachments.map((a) => ({
    key: a.object_key,
    url: "",
    name: a.filename,
    ct: a.content_type,
    size: a.file_size,
    hash: a.content_hash,
    kv: 0,
    sha: a.sha256,
    bh: a.blurhash,
    w: a.width,
    h: a.height,
    ...(a.sensitive ? { sens: true } : {}),
  }));
  return JSON.stringify({ _att: att, ...(text ? { _txt: text } : {}) });
}

/** The grid's images and videos in grid order (#1492). */
export function vaultViewerItems(
  entries: readonly VaultMessage[],
  attachmentId: string,
): ViewerItems {
  const all = vaultAttachments(entries);
  const tapped = all.find((a) => a.id === attachmentId);
  if (!tapped) {
    return { items: [], index: 0 };
  }
  if (!isRollMedia(mediaKind(tapped.content_type))) {
    return { items: [tapped], index: 0 };
  }
  const items = all.filter((a) => isRollMedia(mediaKind(a.content_type)));
  return { items, index: items.indexOf(tapped) };
}

export type VaultSaveFailure = "offline" | "tooLarge" | "other";

/** Port of desktop's `vaultSaveFailure`: "offline" (retry fixes it), "tooLarge" (it does not), or "other". */
export function vaultSaveFailure(err: unknown): VaultSaveFailure {
  const text = err instanceof Error ? err.message : String(err ?? "");
  if (/the limit is \d+ bytes|content_length out of range|blob size out of range/i.test(text)) {
    return "tooLarge";
  }
  if (
    /error sending request|connection|timed out|dns error|network error/i.test(text) ||
    /\b5\d\d [A-Z][a-z]/.test(text)
  ) {
    return "offline";
  }
  return "other";
}
