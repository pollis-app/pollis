// The content string a message carries, parsed: plain text, or the `_att` /
// `_txt` envelope an attachment message wraps its files and caption in. Pure
// (no react-native), so the node tests reach it. The message pages
// (`hooks/queries/useMessages.ts`) and pinned-message snapshots (`lib/pins.ts`,
// which store this exact string shape) both read content through it.

import type { MessageAttachment } from "../types";

// Wire shape of a single attachment inside the `_att` array embedded in
// message content JSON — must stay in lockstep with desktop's parseContent
// (frontend/src/hooks/queries/useMessages.ts) and the writer in
// lib/attachments.ts.
interface AttachmentWire {
  key: string;
  hash: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
}

// Parses structured attachment JSON embedded in message content.
// Plain-text messages pass through as-is. Content with attachments looks
// like: {"_att":[{"key":"media/…","name":"…","ct":"…","size":N,…}],"_txt":"caption"}
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
      attachments: (parsed._att as AttachmentWire[]).map((a) => ({
        id: a.key,
        object_key: a.key,
        content_hash: a.hash,
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
