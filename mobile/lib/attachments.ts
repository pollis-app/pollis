// Attachment upload + message-content envelope: the mobile half of desktop's
// frontend/src/utils/attachmentEnvelope.ts, which it must match byte-for-byte.
// `upload_media` (the Vault's path; chat attachments go through the outbox)
// does the heavy work in Rust; `send_message` re-parses `_att` to register
// references.

import { invoke } from "./native";

export interface PickedAttachment {
  id: string;
  /** `file://` URI; `upload_media` accepts it directly. */
  uri: string;
  name: string;
  mimeType: string;
  width?: number;
  height?: number;
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
  // Plaintext SHA-256, carried only inside the encrypted envelope.
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
  width?: number;
  height?: number;
}

/**
 * Chat attachments go to the durable outbox (#1335), which copies each file
 * into the encrypted local DB with the row, so the upload survives the app
 * being killed.
 */
export function outgoingAttachments(attachments: PickedAttachment[]): OutgoingAttachment[] {
  return attachments.map((att) => ({
    path: att.uri,
    filename: att.name,
    contentType: att.mimeType,
    width: att.width,
    height: att.height,
  }));
}

/** Successful uploads per picked attachment (#1497); weak, so a dropped draft frees them. */
const uploaded = new WeakMap<PickedAttachment, MediaUploadResult>();

/**
 * Upload every attachment, then build the content string. `_txt` is OMITTED
 * when the caption is empty (not ""), as on desktop.
 */
export async function buildMessageContent(
  attachments: PickedAttachment[],
  contentText: string,
): Promise<string> {
  if (attachments.length === 0) {
    return contentText;
  }
  // All uploads settle before a failure is reported. The Vault hands a failed
  // draft back as the SAME objects, so a retry uploads only what failed (#1497).
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
    _att: results.map((r) => ({
      key: r.key,
      url: r.url,
      name: r.filename,
      ct: r.content_type,
      size: r.size_bytes,
      hash: r.content_hash,
      kv: r.key_version,
      sha: r.sha256,
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
