// Pure rules behind the Vault screen (#107, mobile #1316). No react-native or
// expo imports, so tests/vault.test.ts pins them under plain node. Each one
// is a port of the same rule in desktop's frontend/src/pages/Vault.tsx.

import type { MessageAttachment } from "../types";
import { parseContent, type AttachmentWire } from "./messageContent.ts";
import { isRollMedia, mediaKind, type ViewerItems } from "./media/viewer.ts";

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
    // Kept across a caption edit (#1568), as desktop's Vault keeps it.
    ...(a.sensitive ? { sens: true } : {}),
  }));
  return JSON.stringify({ _att: att, ...(text ? { _txt: text } : {}) });
}

/**
 * The full-screen viewer's pages for an attachment tapped in the Vault's
 * media view (#1492): the grid's own images and videos, in the grid's order
 * (newest entry first), so a swipe walks exactly what the grid shows — as
 * desktop's MediaGalleryView walks its roll. Audio and other files open
 * alone; an id the vault no longer holds yields nothing.
 */
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

/** Why a save failed, as far as the user can act on it (#1497). */
export type VaultSaveFailure = "offline" | "tooLarge" | "other";

/**
 * Sort a failed save into what the user can do about it. The core's errors
 * reach JS as text, so this reads the shapes `pollis-core` produces: a
 * connection that failed or timed out, or a 5xx from the DS or R2, is
 * "offline" (sending again is the fix); a file over the upload cap is
 * "tooLarge" (sending again is not). Anything else stays generic. A port of
 * desktop's `vaultSaveFailure` (frontend/src/utils/vaultSaveFailure.ts).
 */
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
