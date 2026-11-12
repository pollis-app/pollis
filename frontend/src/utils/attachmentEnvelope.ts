import { invoke, discardStagedAttachment } from "../bridge";
import { blobToBase64 } from "./imageProcessing";
import type { Attachment } from "../components/ui/ChatInput";

type MediaUploadResult = {
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
  /** A video's bounded poster JPEG, base64 (#1837): `pv`. */
  poster?: string;
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
  /** A video's captured frame, base64 (#1837); the core makes the poster, blurhash and size from it. */
  poster?: string;
};

/** A video's captured frame as base64, or undefined; a frame that will not read just sends without one. */
async function posterOf(att: Attachment): Promise<string | undefined> {
  if (!att.mimeType.startsWith("video/") || !att.posterFrame) {
    return undefined;
  }
  return blobToBase64(att.posterFrame).catch(() => undefined);
}

/** Hand attachments to the durable outbox (#1335); a video's frame goes with it (#1837). */
export async function outgoingAttachments(
  attachments: Attachment[],
): Promise<OutgoingAttachment[]> {
  const out: OutgoingAttachment[] = [];
  for (const att of attachments) {
    if (!att.source) {
      throw new Error(`attachment ${att.name} has no bytes to send yet`);
    }
    const poster = await posterOf(att);
    out.push({
      ...(att.source.kind === "staged" ? { stagedId: att.source.id } : { path: att.source.path }),
      filename: att.name,
      contentType: att.mimeType,
      // The composer's sensitive-cover blurhash (#1568) until the upload's or poster's own.
      blurhash: att.blurhash,
      ...(att.sensitive ? { sensitive: true } : {}),
      ...(poster ? { poster } : {}),
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

  // A file path or staged bytes. The caller owns retries (#1497); all uploads settle before failing.
  const settled = await Promise.allSettled(
    attachments.map(async (att) => {
      const done = uploaded.get(att);
      if (done) {
        return done;
      }
      if (!att.source) {
        throw new Error(`attachment ${att.name} has no bytes to upload yet`);
      }
      // Images get their blurhash during upload; a video's comes from its frame (#1837).
      const poster = (await posterOf(att)) ?? null;
      const upload =
        att.source.kind === "staged"
          ? invoke<MediaUploadResult>("upload_media_staged", {
              stagedId: att.source.id,
              filename: att.name,
              contentType: att.mimeType,
              poster,
            })
          : invoke<MediaUploadResult>("upload_media", {
              path: att.source.path,
              filename: att.name,
              contentType: att.mimeType,
              poster,
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
    _att: results.map((r, i) => ({
      key: r.key,
      url: r.url,
      name: r.filename,
      ct: r.content_type,
      size: r.size_bytes,
      hash: r.content_hash,
      kv: r.key_version,
      sha: r.sha256,
      bh: r.blurhash ?? attachments[i]?.blurhash,
      w: r.width,
      h: r.height,
      ...(attachments[i]?.sensitive ? { sens: true } : {}),
      ...(r.poster ? { pv: r.poster } : {}),
    })),
  };
  if (contentText) {
    envelope._txt = contentText;
  }
  return JSON.stringify(envelope);
}
