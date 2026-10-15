import { invoke, discardStagedAttachment } from "../bridge";
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
 * An attachment as `send_message` takes it when the durable outbox uploads it
 * itself (#1335). Mirrors `pollis_core::commands::messages::OutgoingAttachment`:
 * exactly one of `path` / `stagedId`.
 */
export type OutgoingAttachment = {
  path?: string;
  stagedId?: string;
  filename: string;
  contentType: string;
  blurhash?: string;
  width?: number;
  height?: number;
};

/**
 * Hand a composer's attachments to the outbox instead of uploading them here
 * (#1335). The message row is on screen from the moment the user sends, so
 * the upload has to be as durable as the send: the core reads the bytes (a
 * file the user chose, or the staged paste) into its encrypted local database
 * together with the row, before any network work, and the outbox uploads them
 * — after a restart if it has to. Only a video's poster blurhash is still
 * worked out here, because only the renderer has the poster frame.
 */
export async function outgoingAttachments(
  attachments: Attachment[],
): Promise<OutgoingAttachment[]> {
  const out: OutgoingAttachment[] = [];
  for (const att of attachments) {
    if (!att.source) {
      throw new Error(`attachment ${att.name} has no bytes to send yet`);
    }
    const meta =
      att.mimeType.startsWith("video/") && att.preview
        ? await blurhashFromUrl(att.preview).catch(() => null)
        : null;
    out.push({
      ...(att.source.kind === "staged" ? { stagedId: att.source.id } : { path: att.source.path }),
      filename: att.name,
      contentType: att.mimeType,
      blurhash: meta?.hash,
      width: meta?.width,
      height: meta?.height,
    });
  }
  return out;
}

/**
 * Release the staged bytes of a send the core never took (it failed before
 * storing them). Once stored they are already released; a second discard is a
 * no-op.
 */
export async function releaseStaged(attachments: Attachment[]): Promise<void> {
  await Promise.all(
    attachments.map((att) =>
      att.source?.kind === "staged"
        ? discardStagedAttachment(att.source.id).catch(() => false)
        : Promise.resolve(false),
    ),
  );
}

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

  // Two upload commands, one per source kind, because there is exactly one
  // place an attachment's bytes can be: a file the user already has, or the
  // in-memory staging area (`pollis_core::commands::staging`). A successful
  // staged upload releases its own bytes; a failed one leaves them staged so a
  // retry still has something to send.
  let results: MediaUploadResult[];
  try {
    results = await Promise.all(
      attachments.map((att) => {
        if (!att.source) {
          return Promise.reject(
            new Error(`attachment ${att.name} has no bytes to upload yet`),
          );
        }
        if (att.source.kind === "staged") {
          return invoke<MediaUploadResult>("upload_media_staged", {
            stagedId: att.source.id,
            filename: att.name,
            contentType: att.mimeType,
          });
        }
        return invoke<MediaUploadResult>("upload_media", {
          path: att.source.path,
          filename: att.name,
          contentType: att.mimeType,
        });
      }),
    );
  } catch (err) {
    // The send has failed and the composer cleared itself the moment it handed
    // these over, so nothing will retry them — release the bytes rather than
    // leaving entries no reference points at. Uploads that already succeeded
    // released their own; discarding an id twice is a no-op.
    await Promise.all(
      attachments.map((att) =>
        att.source?.kind === "staged"
          ? discardStagedAttachment(att.source.id).catch(() => false)
          : Promise.resolve(false),
      ),
    );
    throw err;
  }

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
