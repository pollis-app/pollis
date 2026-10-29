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
  /** The object's random file id (#1382), not a content hash. */
  content_hash: string;
  /** Which ring key sealed it; always `0` for these Vault uploads. */
  key_version: number;
  /** Plaintext SHA-256, carried only inside the encrypted envelope. */
  sha256: string;
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
  /** Marked sensitive in the composer (#1568). */
  sensitive?: boolean;
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
      // An image's own blurhash comes from the upload; the one the composer
      // drew a sensitive cover with (#1568) stands in until then.
      blurhash: meta?.hash ?? att.blurhash,
      width: meta?.width,
      height: meta?.height,
      ...(att.sensitive ? { sensitive: true } : {}),
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
 * Uploads that already succeeded, by the composer's attachment object (#1497).
 * A failed draft goes back to the composer as the SAME objects, so a retry
 * finds these and uploads only what failed. Weak, so a draft that is dropped
 * takes its entries with it.
 */
const uploaded = new WeakMap<Attachment, MediaUploadResult>();

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
  // retry still has something to send. The caller owns that retry: the Vault
  // hands the failed draft back to its composer (#1497), and the composer
  // releases staged bytes when a card is removed — so nothing is discarded
  // here on failure.
  // Every upload runs to completion before a failure is reported, and each one
  // that succeeded is remembered against its attachment (#1497): a retry of
  // the same draft uploads only what failed. That is also what makes a retry
  // possible at all for a staged attachment, whose bytes are released the
  // moment its upload succeeds.
  const settled = await Promise.allSettled(
    attachments.map((att) => {
      const done = uploaded.get(att);
      if (done) {
        return Promise.resolve(done);
      }
      if (!att.source) {
        return Promise.reject(
          new Error(`attachment ${att.name} has no bytes to upload yet`),
        );
      }
      const upload =
        att.source.kind === "staged"
          ? invoke<MediaUploadResult>("upload_media_staged", {
              stagedId: att.source.id,
              filename: att.name,
              contentType: att.mimeType,
            })
          : invoke<MediaUploadResult>("upload_media", {
              path: att.source.path,
              filename: att.name,
              contentType: att.mimeType,
            });
      return upload.then((result) => {
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
    _att: results.map((r, i) => {
      const vMeta = videoBlurhashes.get(attachments[i]?.id ?? "");
      return {
        key: r.key,
        url: r.url,
        name: r.filename,
        ct: r.content_type,
        size: r.size_bytes,
        hash: r.content_hash,
        kv: r.key_version,
        sha: r.sha256,
        bh: r.blurhash ?? vMeta?.bh,
        w: r.width ?? vMeta?.w,
        h: r.height ?? vMeta?.h,
        // Only when set, as the core's outbox writes it (#1568).
        ...(attachments[i]?.sensitive ? { sens: true } : {}),
      };
    }),
  };
  if (contentText) {
    envelope._txt = contentText;
  }
  return JSON.stringify(envelope);
}
