import { invoke, discardStagedAttachment } from "../bridge";
import { blurhashFromUrl } from "./imageProcessing";
import type { Attachment } from "../components/ui/ChatInput";

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
  /** Plaintext SHA-256, only inside the encrypted envelope. */
  sha256: string;
  blurhash?: string;
  width?: number;
  height?: number;
};

/** Mirrors `pollis_core::commands::messages::OutgoingAttachment` (#1335): exactly one of `path` / `stagedId`. */
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

/** Hand attachments to the durable outbox (#1335); only a video's poster blurhash is computed here. */
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
      // The composer's sensitive-cover blurhash (#1568) until the upload's own.
      blurhash: meta?.hash ?? att.blurhash,
      width: meta?.width,
      height: meta?.height,
      ...(att.sensitive ? { sensitive: true } : {}),
    });
  }
  return out;
}

/** Release staged bytes of a send the core never stored; idempotent. */
export async function releaseStaged(attachments: Attachment[]): Promise<void> {
  await Promise.all(
    attachments.map((att) =>
      att.source?.kind === "staged"
        ? discardStagedAttachment(att.source.id).catch(() => false)
        : Promise.resolve(false),
    ),
  );
}

/** Succeeded uploads by attachment (#1497), so a retry uploads only what failed. */
const uploaded = new WeakMap<Attachment, MediaUploadResult>();

/** Upload attachments and encode the `_att` envelope (decoded in `hooks/queries/useMessages.ts`). */
export async function buildMessageContent(
  attachments: Attachment[],
  contentText: string,
): Promise<string> {
  if (attachments.length === 0) {
    return contentText;
  }

  // Video blurhash from the local poster; images get theirs during upload.
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

  // A file path or staged bytes. The caller owns retries (#1497); all uploads settle before failing.
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
        ...(attachments[i]?.sensitive ? { sens: true } : {}),
      };
    }),
  };
  if (contentText) {
    envelope._txt = contentText;
  }
  return JSON.stringify(envelope);
}
