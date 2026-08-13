import { invoke } from "../bridge";
import { blurhashFromUrl } from "./imageProcessing";
import type { Attachment } from "../components/ui/ChatInput";

/** Shape `upload_media` returns. Mirrors the Rust command's result struct. */
export type MediaUploadResult = {
  key: string;
  url: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  content_hash: string;
  blurhash?: string;
  width?: number;
  height?: number;
};

/**
 * Upload a composer's attachments and encode them into the message body.
 *
 * This is the ENCODE half of the `_att` envelope that
 * `hooks/queries/useMessages.ts` decodes — the two must move together, so the
 * shape is documented there once and not restated here.
 *
 * Extracted from `MainContent`'s send handler when threads (#825) gained their
 * own composer: the channel and the thread both upload and encode identically,
 * and the alternative was a second copy that would drift the moment the wire
 * shape changed. Deliberately NOT the whole send path — the optimistic-update
 * machinery around it is keyed to the channel's query and is genuinely
 * channel-only, since a thread reply lands in its own query.
 *
 * Returns the string to send as message content: the plain text when there are
 * no attachments, otherwise the JSON envelope carrying them plus the caption.
 */
export async function buildMessageContent(
  attachments: Attachment[],
  contentText: string,
): Promise<string> {
  if (attachments.length === 0) {
    return contentText;
  }

  // Derive a blurhash for videos from the locally-captured poster frame, so a
  // receiver sees a placeholder without downloading the video first. Images get
  // theirs server-side during upload.
  const videoBlurhashes = new Map<string, { bh: string; w: number; h: number }>();
  await Promise.all(
    attachments
      .filter((att) => att.mimeType.startsWith("video/") && att.preview)
      .map(async (att) => {
        const meta = await blurhashFromUrl(att.preview!).catch(() => null);
        if (meta) {
          videoBlurhashes.set(att.id, {
            bh: meta.hash,
            w: meta.width,
            h: meta.height,
          });
        }
      }),
  );

  const results = await Promise.all(
    attachments.map((att) =>
      invoke<MediaUploadResult>("upload_media", {
        path: att.path,
        filename: att.name,
        contentType: att.mimeType,
      }),
    ),
  );

  const envelope: Record<string, unknown> = {
    _att: results.map((r, i) => {
      const vMeta = videoBlurhashes.get(attachments[i]?.id ?? "");
      return {
        key: r.key,
        url: r.url,
        name: r.filename,
        ct: r.content_type,
        size: r.size_bytes,
        hash: r.content_hash,
        bh: r.blurhash ?? vMeta?.bh,
        w: r.width ?? vMeta?.w,
        h: r.height ?? vMeta?.h,
      };
    }),
  };
  if (contentText) {
    envelope._txt = contentText;
  }
  return JSON.stringify(envelope);
}
