// Pure rules behind the media viewer (#1248); no react-native imports.

import type { MessageAttachment } from "../../types";

export type MediaKind = "image" | "video" | "audio" | "file";

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

/** The photo library takes stills and video only (iOS rejects audio). */
export function canSaveToLibrary(kind: MediaKind): boolean {
  return kind === "image" || kind === "video";
}

/** Images and videos walk with their neighbours; audio and files open alone. */
export function isRollMedia(kind: MediaKind): boolean {
  return kind === "image" || kind === "video";
}

// AVPlayer and the iOS photo library pick the decoder by extension.
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

export function extensionFor(contentType: string): string | null {
  const base = contentType.split(";")[0].trim().toLowerCase();
  return EXTENSIONS[base] ?? null;
}

/** The sender's filename made safe for one path segment, with an extension; falls back to the hash. */
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
  // 255-byte filesystems, with room for an extension.
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

/** An image or video walks the conversation's images and videos, oldest first. */
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

// A viewer opened on a pending copy follows its id to the confirmed R2 key.
const attachmentAliases = new Map<string, string>();

export function aliasAttachment(from: string, to: string): void {
  if (from && to && from !== to) {
    attachmentAliases.set(from, to);
  }
}

export function currentAttachmentId(id: string): string {
  let next = id;
  // Bounded: an alias never points back at a pending id, but don't trust it.
  for (let i = 0; i < 4 && attachmentAliases.has(next); i++) {
    next = attachmentAliases.get(next) as string;
  }
  return next;
}

// Swipe down to dismiss (#1492): past a fifth of the page, or a fling. A worklet (inert under node).
export const DISMISS_FRACTION = 0.2;
export const DISMISS_VELOCITY = 900;

export function shouldDismiss(dy: number, vy: number, pageHeight: number): boolean {
  "worklet";
  // Nothing pulled down, or heading back up when released.
  if (dy <= 0 || vy < -DISMISS_VELOCITY / 3) {
    return false;
  }
  return dy > pageHeight * DISMISS_FRACTION || vy > DISMISS_VELOCITY;
}

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
