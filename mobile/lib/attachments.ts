// Attachment upload + message-content envelope; must match desktop's attachmentEnvelope.ts byte-for-byte.

import { invoke } from "./native";

export interface PickedAttachment {
  id: string;
  uri: string;
  name: string;
  mimeType: string;
  width?: number;
  height?: number;
  /** Marked sensitive (#1568): sent with `sens`. */
  sensitive?: boolean;
  /** The cover's blurhash; covers a video (no core blurhash) in colour too. An image's own wins. */
  blurhash?: string;
}

export interface MediaUploadResult {
  key: string;
  url: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  // The object's random file id (#1382), not a content hash.
  content_hash: string;
  // Which ring key sealed it; always 0 for these Vault uploads.
  key_version: number;
  sha256: string;
  blurhash?: string | null;
  width?: number | null;
  height?: number | null;
}

/** Mirrors `pollis_core::commands::messages::OutgoingAttachment` (#1335). */
export interface OutgoingAttachment {
  path: string;
  filename: string;
  contentType: string;
  blurhash?: string;
  width?: number;
  height?: number;
  sensitive?: boolean;
}

/** Hand picked attachments to the outbox (#1335), so the upload survives the app being killed. */
export function outgoingAttachments(attachments: PickedAttachment[]): OutgoingAttachment[] {
  return attachments.map((att) => ({
    path: att.uri,
    filename: att.name,
    contentType: att.mimeType,
    width: att.width,
    height: att.height,
    ...(att.blurhash ? { blurhash: att.blurhash } : {}),
    ...(att.sensitive ? { sensitive: true } : {}),
  }));
}

/** Uploads that already succeeded (#1497); weak, so a dropped draft takes its entries along. */
const uploaded = new WeakMap<PickedAttachment, MediaUploadResult>();

/** Upload every attachment, then build desktop's exact envelope; `_txt` is OMITTED when empty. */
export async function buildMessageContent(
  attachments: PickedAttachment[],
  contentText: string,
): Promise<string> {
  if (attachments.length === 0) {
    return contentText;
  }
  // Each success is remembered (#1497), so a retry uploads only what failed.
  const settled = await Promise.allSettled(
    attachments.map((att) => {
      const done = uploaded.get(att);
      if (done) {
        return Promise.resolve(done);
      }
      return invoke<MediaUploadResult>("upload_media", {
        path: att.uri,
        filename: att.name,
        contentType: att.mimeType,
      }).then((result) => {
        uploaded.set(att, result);
        return result;
      });
    }),
  );
  const failed = settled.find((s): s is PromiseRejectedResult => s.status === "rejected");
  if (failed) {
    throw failed.reason;
  }
  const results = settled.map((s) => (s as PromiseFulfilledResult<MediaUploadResult>).value);

  const envelope: Record<string, unknown> = {
    _att: results.map((r, i) => ({
      key: r.key,
      url: r.url,
      name: r.filename,
      ct: r.content_type,
      size: r.size_bytes,
      hash: r.content_hash,
      kv: r.key_version,
      sha: r.sha256,
      bh: r.blurhash ?? attachments[i]?.blurhash ?? undefined,
      w: r.width ?? undefined,
      h: r.height ?? undefined,
      // Only when set, as the core and desktop write it (#1568).
      ...(attachments[i]?.sensitive ? { sens: true } : {}),
    })),
  };
  if (contentText) {
    envelope._txt = contentText;
  }
  return JSON.stringify(envelope);
}
