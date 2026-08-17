// Attachment upload + message-content envelope (mobile half of desktop's
// frontend/src/utils/attachmentEnvelope.ts — the envelope must match
// byte-for-byte so cross-platform messages parse everywhere).
//
// `upload_media` does everything heavy in Rust: read the file, SHA-256,
// convergent AES-GCM encrypt, presigned PUT via the DS, dedup, blurhash +
// dimensions for images. Reference registration happens inside Rust
// `send_message` by re-parsing the `_att` JSON, so sending the same
// envelope string keeps refcounting working for free.

import { invoke } from "./native";

/** An image the user picked but hasn't sent yet. */
export interface PickedAttachment {
  id: string;
  /** `file://` URI from the picker — `upload_media` accepts it directly. */
  uri: string;
  name: string;
  mimeType: string;
  width?: number;
  height?: number;
}

/** Mirror of `pollis_core::commands::r2::MediaUploadResult`. */
export interface MediaUploadResult {
  key: string;
  url: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  content_hash: string;
  blurhash?: string | null;
  width?: number | null;
  height?: number | null;
}

/**
 * Upload every attachment, then build the message content string. With no
 * attachments the caption passes through untouched (plain-text fast path).
 * Matches desktop's envelope exactly: `_att` entries carry
 * {key, url, name, ct, size, hash, bh, w, h}; `_txt` is OMITTED entirely
 * when the caption is empty (not "").
 */
export async function buildMessageContent(
  attachments: PickedAttachment[],
  contentText: string,
): Promise<string> {
  if (attachments.length === 0) {
    return contentText;
  }
  const results = await Promise.all(
    attachments.map((att) =>
      invoke<MediaUploadResult>("upload_media", {
        path: att.uri,
        filename: att.name,
        contentType: att.mimeType,
      }),
    ),
  );

  const envelope: Record<string, unknown> = {
    _att: results.map((r) => ({
      key: r.key,
      url: r.url,
      name: r.filename,
      ct: r.content_type,
      size: r.size_bytes,
      hash: r.content_hash,
      bh: r.blurhash ?? undefined,
      w: r.width ?? undefined,
      h: r.height ?? undefined,
    })),
  };
  if (contentText) {
    envelope._txt = contentText;
  }
  return JSON.stringify(envelope);
}
