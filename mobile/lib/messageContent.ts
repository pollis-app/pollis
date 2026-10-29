// The message content string, both directions (pure): plain text or the `_att`/`_txt` envelope, in lockstep with desktop's parseContent.

import type { AttachmentKeyScope, ContentOrigin, MessageAttachment } from "../types";

export interface AttachmentWire {
  // Absent on an entry the outbox has not uploaded yet (#1335).
  key?: string;
  url?: string;
  hash?: string;
  // Key ring version; 0 in the Vault.
  kv?: number;
  sha?: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
  sens?: boolean;
}

/** Null when it cannot be opened from here: uploading, pre-#1382, or preview-only. */
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

/** Split content into caption and attachments; `origin` names each key. */
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
      // No `key`: not uploaded by the outbox yet (#1335), drawn pending.
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
