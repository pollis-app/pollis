// Picked attachments and what the outbox takes for them (#1335): the core uploads them, chat and Vault alike (#2100).

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
  /** A video's frame from the platform decoder, base64 JPEG (#1837); the core makes the poster from it. */
  posterFrame?: string;
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
  poster?: string;
}

/** A video's frame, if the composer got one; never for anything else. */
function posterFrameOf(att: PickedAttachment): string | undefined {
  return att.mimeType.startsWith("video/") ? att.posterFrame : undefined;
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
    ...(posterFrameOf(att) ? { poster: posterFrameOf(att) } : {}),
  }));
}
