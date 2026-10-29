// The message content string, both directions. Pure (no react-native), so
// node tests can pin it. Chat messages, pinned-message snapshots (`lib/pins.ts`)
// and Vault entries (#107) share this
// one shape: plain text, or the attachment envelope
// `{"_att":[{"key":"media/…","name":"…","ct":"…","size":N,…}],"_txt":"caption"}`.
//
// Must stay in lockstep with desktop's parseContent
// (frontend/src/hooks/queries/useMessages.ts) and the writer in
// lib/attachments.ts.

import type { AttachmentKeyScope, ContentOrigin, MessageAttachment } from "../types";

// Wire shape of a single attachment inside the `_att` array.
export interface AttachmentWire {
  // Absent on an entry the outbox has not uploaded yet (#1335).
  key?: string;
  url?: string;
  // The object's random file id (#1382).
  hash?: string;
  // Which key of the conversation's file key ring sealed it; 0 in the Vault.
  kv?: number;
  // Plaintext SHA-256, verified after decrypt.
  sha?: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
  // Sensitive media (#1568); omitted unless set.
  sens?: boolean;
}

/** The key an attachment reference names, or null when it cannot be opened
 *  from here: uploading, a pre-#1382 reference, or a preview-only origin. */
function keyScopeFor(a: AttachmentWire, origin: ContentOrigin): AttachmentKeyScope | null {
  if (origin === null || !a.key || !a.sha || typeof a.kv !== "number") {
    return null;
  }
  if (origin === "vault") {
    return a.kv === 0 ? { kind: "vault" } : null;
  }
  return a.kv >= 1
    ? { kind: "conversation", conversation_id: origin.conversationId, key_version: a.kv }
    : null;
}

/** Split a content string into its caption and attachments. Plain-text
 *  content passes through as-is. `origin` names each attachment's key. */
export function parseContent(raw: string | undefined, origin: ContentOrigin): {
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
        sha256: a.sha ?? "",
        key_scope: keyScopeFor(a, origin),
        filename: a.name,
        content_type: a.ct,
        file_size: a.size,
        uploaded_at: Date.now(),
        blurhash: a.bh,
        width: a.w,
        height: a.h,
        sensitive: a.sens === true,
      })),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}
