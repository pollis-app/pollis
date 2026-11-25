import { discardStagedAttachment } from "../bridge";
import { blobToBase64 } from "./imageProcessing";
import type { Attachment } from "../components/ui/ChatInput";

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
