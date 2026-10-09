// Pure rules behind the full-screen media viewer (#1248). No react-native or
// expo imports, so `tests/media-viewer.test.ts` can pin them under plain node.

import type { MessageAttachment } from "../../types";

export type MediaKind = "image" | "video" | "audio" | "file";

/** What the viewer draws for an attachment, from its MIME type. */
export function mediaKind(contentType: string): MediaKind {
  if (contentType.startsWith("image/")) {
    return "image";
  }
  if (contentType.startsWith("video/")) {
    return "video";
  }
  if (contentType.startsWith("audio/")) {
    return "audio";
  }
  return "file";
}

/**
 * Whether "Save to photos" applies. The photo library takes stills and
 * video only (iOS rejects audio outright); everything else goes through the
 * share sheet.
 */
export function canSaveToLibrary(kind: MediaKind): boolean {
  return kind === "image" || kind === "video";
}

/**
 * Whether the item walks with its neighbours. Desktop's roll
 * (MediaGalleryView) holds images and videos; audio and files open alone,
 * as desktop's own audio lightbox does.
 */
export function isRollMedia(kind: MediaKind): boolean {
  return kind === "image" || kind === "video";
}

// Common extensions per MIME type, for a filename that arrived without one.
// The photo library (iOS) and AVPlayer pick the decoder by extension, and the
// decrypted cache file is named by content hash alone.
const EXTENSIONS: Record<string, string> = {
  "image/jpeg": "jpg",
  "image/jpg": "jpg",
  "image/png": "png",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/heic": "heic",
  "image/heif": "heif",
  "image/avif": "avif",
  "image/bmp": "bmp",
  "image/tiff": "tiff",
  "video/mp4": "mp4",
  "video/quicktime": "mov",
  "video/webm": "webm",
  "video/x-matroska": "mkv",
  "video/3gpp": "3gp",
  "video/x-m4v": "m4v",
  "audio/mpeg": "mp3",
  "audio/mp3": "mp3",
  "audio/mp4": "m4a",
  "audio/x-m4a": "m4a",
  "audio/aac": "aac",
  "audio/wav": "wav",
  "audio/x-wav": "wav",
  "audio/ogg": "ogg",
  "audio/webm": "webm",
  "audio/flac": "flac",
  "application/pdf": "pdf",
  "application/zip": "zip",
  "text/plain": "txt",
};

/** The extension a MIME type implies, or null when it implies none. */
export function extensionFor(contentType: string): string | null {
  const base = contentType.split(";")[0].trim().toLowerCase();
  return EXTENSIONS[base] ?? null;
}

/**
 * The name the exported copy carries into the photo library or the share
 * sheet: the sender's filename, made safe for one path segment, with an
 * extension the platform can type it by. Falls back to the content hash when
 * the filename is unusable.
 */
export function exportFilename(
  filename: string,
  contentType: string,
  contentHash: string,
): string {
  // Path separators, control characters and the characters Android's
  // MediaStore or FAT-backed shares reject.
  const cleaned = filename
    .replace(/[\u0000-\u001f\u007f]/g, "")
    .replace(/[\\/:*?"<>|]/g, "_")
    .trim()
    .replace(/^\.+/, "");
  const fallback = `pollis-${contentHash.slice(0, 12) || "file"}`;
  let name = cleaned.length > 0 ? cleaned : fallback;
  // Keep the name short enough for every filesystem (255 bytes), leaving
  // room for an appended extension.
  if (name.length > 120) {
    const dot = name.lastIndexOf(".");
    const ext = dot > 0 && name.length - dot <= 10 ? name.slice(dot) : "";
    name = name.slice(0, 120 - ext.length) + ext;
  }
  const hasExtension = /\.[A-Za-z0-9]{1,10}$/.test(name);
  if (hasExtension) {
    return name;
  }
  const ext = extensionFor(contentType);
  return ext ? `${name}.${ext}` : name;
}

/** The fields of a message the viewer reads. */
export interface ViewerMessage {
  id: string;
  attachments?: MessageAttachment[];
  deleted_at?: number;
  created_at: number;
}

export interface ViewerItems {
  items: MessageAttachment[];
  index: number;
}

/**
 * The viewer's pages for a tapped attachment. An image or video walks the
 * conversation's images and videos in timeline order (oldest first, so a
 * swipe towards the end goes forward in time, like the chat). Audio and other
 * files open alone. `messages` may arrive in any order; an attachment that is
 * not found yields no items.
 */
export function collectViewerItems(
  messages: readonly ViewerMessage[],
  attachmentId: string,
): ViewerItems {
  let tapped: MessageAttachment | null = null;
  for (const message of messages) {
    for (const attachment of message.attachments ?? []) {
      if (attachment.id === attachmentId) {
        tapped = attachment;
      }
    }
  }
  if (!tapped) {
    return { items: [], index: 0 };
  }
  if (!isRollMedia(mediaKind(tapped.content_type))) {
    return { items: [tapped], index: 0 };
  }

  const ordered = [...messages]
    .filter((m) => !m.deleted_at)
    .sort((a, b) => a.created_at - b.created_at);
  const seen = new Set<string>();
  const items: MessageAttachment[] = [];
  for (const message of ordered) {
    for (const attachment of message.attachments ?? []) {
      if (seen.has(attachment.id)) {
        continue;
      }
      if (!isRollMedia(mediaKind(attachment.content_type))) {
        continue;
      }
      seen.add(attachment.id);
      items.push(attachment);
    }
  }
  const index = items.findIndex((a) => a.id === attachmentId);
  if (index < 0) {
    // The tapped item sits on a deleted message the list still showed.
    return { items: [tapped], index: 0 };
  }
  return { items, index };
}

// A picked attachment is on screen (optimistic send) under the picker's id;
// once the send settles the confirmed message carries the R2 key instead. A
// viewer opened on the pending copy follows the id across that swap.
const attachmentAliases = new Map<string, string>();

/** Record that attachment `from` (a pending copy) is now `to`. */
export function aliasAttachment(from: string, to: string): void {
  if (from && to && from !== to) {
    attachmentAliases.set(from, to);
  }
}

/** The attachment's current id, following any pending → confirmed swap. */
export function currentAttachmentId(id: string): string {
  let next = id;
  // Bounded: an alias never points back at a pending id, but don't trust it.
  for (let i = 0; i < 4 && attachmentAliases.has(next); i++) {
    next = attachmentAliases.get(next) as string;
  }
  return next;
}

/** "1:05" / "1:02:09" for the audio player's clock. */
export function formatClock(seconds: number): string {
  const total = Number.isFinite(seconds) && seconds > 0 ? Math.floor(seconds) : 0;
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  if (h > 0) {
    return `${h}:${String(m).padStart(2, "0")}:${ss}`;
  }
  return `${m}:${ss}`;
}
