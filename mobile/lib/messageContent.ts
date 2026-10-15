// The message content string, both directions. Pure (no react-native), so
// node tests can pin it. Chat messages, pinned-message snapshots (`lib/pins.ts`)
// and Vault entries (#107) share this
// one shape: plain text, or the attachment envelope
// `{"_att":[{"key":"media/…","name":"…","ct":"…","size":N,…}],"_txt":"caption"}`.
//
// Must stay in lockstep with desktop's parseContent
// (frontend/src/hooks/queries/useMessages.ts) and the writer in
// lib/attachments.ts.

import type { MessageAttachment } from "../types";

// Wire shape of a single attachment inside the `_att` array.
export interface AttachmentWire {
  // Absent on an entry the outbox has not uploaded yet (#1335).
  key?: string;
  url?: string;
  hash?: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
}

/** Split a content string into its caption and attachments. Plain-text
 *  content passes through as-is. */
export function parseContent(raw: string | undefined): {
  text: string;
  attachments: MessageAttachment[];
} {
  if (!raw?.startsWith("{")) {
    return { text: raw ?? "", attachments: [] };
  }
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed._att)) {
      // Legacy `_txt`-only envelopes still reduce to their caption.
      if (typeof parsed?._txt === "string") {
        return { text: parsed._txt, attachments: [] };
      }
      return { text: raw, attachments: [] };
    }
    return {
      text: typeof parsed._txt === "string" ? parsed._txt : "",
      // An entry with no `key` is one the outbox has not uploaded yet (#1335):
      // drawn like an optimistic stub's attachment, pending.
      attachments: (parsed._att as AttachmentWire[]).map((a, i) => ({
        id: a.key ?? `uploading-${i}`,
        object_key: a.key ?? "",
        content_hash: a.hash ?? "",
        filename: a.name,
        content_type: a.ct,
        file_size: a.size,
        uploaded_at: Date.now(),
        blurhash: a.bh,
        width: a.w,
        height: a.h,
      })),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}
