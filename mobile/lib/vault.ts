// Pure rules behind the Vault screen (#107, mobile #1316). No react-native or
// expo imports, so tests/vault.test.ts pins them under plain node. Each one
// is a port of the same rule in desktop's frontend/src/pages/Vault.tsx.

import type { MessageAttachment } from "../types";
import { parseContent, type AttachmentWire } from "./messageContent.ts";

/** Mirror of `pollis_core::commands::vault::VaultMessage`. */
export interface VaultMessage {
  id: string;
  /** Same content-string shape as chat messages — plain text or the
   *  `{"_att":[...],"_txt":"..."}` envelope; parse with `parseContent`. */
  content: string;
  pinned: boolean;
  created_at: string;
  updated_at: string;
}

/** Whether an entry was rewritten after it was made. Pin/unpin is a rewrite
 *  too (the flag lives inside the ciphertext), exactly as on desktop. */
export function isVaultEntryEdited(entry: VaultMessage): boolean {
  return entry.updated_at !== entry.created_at;
}

/**
 * Client-side search over decrypted entries — the server cannot search what
 * it cannot read. Case-insensitive substring over the caption and every
 * attachment's filename; a blank query keeps every entry. Order is kept.
 */
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

/** Every attachment across the vault, newest entry first, each object once —
 *  the media view. `entries` arrive oldest first, as the core returns them. */
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

/**
 * The content string an edit saves, or null when there is nothing to save.
 * An entry with files keeps them across a text edit: the same envelope is
 * rebuilt around the new caption (with `_txt` omitted when it is empty, as
 * `buildMessageContent` does). A text-only entry saves the trimmed text.
 */
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
  }));
  return JSON.stringify({ _att: att, ...(text ? { _txt: text } : {}) });
}

/** The entries as the media viewer's message list (lib/media/viewer.ts). */
export function vaultViewerMessages(entries: readonly VaultMessage[]): {
  id: string;
  attachments: MessageAttachment[];
  created_at: number;
}[] {
  return entries.map((entry) => ({
    id: entry.id,
    attachments: parseContent(entry.content, "vault").attachments,
    created_at: new Date(entry.created_at).getTime(),
  }));
}
