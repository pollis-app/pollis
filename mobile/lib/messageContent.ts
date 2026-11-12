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
  // A video's poster JPEG, base64 (#1837); validated by `posterOf`.
  pv?: unknown;
}

/** Longest `pv` accepted; the core sends at most 16 KiB of it. */
const POSTER_MAX_B64 = 64 * 1024;

/** A sender's `pv` (#1837) only if it is plain base64 of a sane size; anything else is ignored. As desktop's. */
export function posterOf(pv: unknown): string | undefined {
  if (typeof pv !== "string" || pv.length === 0 || pv.length > POSTER_MAX_B64) {
    return undefined;
  }
  return /^[A-Za-z0-9+/]+={0,2}$/.test(pv) ? pv : undefined;
}

/** An image source for a base64 JPEG poster. */
export function posterUri(poster: string): string {
  return `data:image/jpeg;base64,${poster}`;
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
        poster: posterOf(a.pv),
      })),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}
