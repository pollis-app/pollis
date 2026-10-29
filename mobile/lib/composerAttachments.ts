// Pure rules behind the composer's pending-attachment previews (#1585).

import type { MessageAttachment } from "../types";
import type { PickedAttachment } from "./attachments";
import { collectViewerItems, mediaKind, type MediaKind, type ViewerItems } from "./media/viewer.ts";

/**
/** Desktop's 80px plus half (#1568). */
export const COMPOSER_PREVIEW_SIZE = 120;

export function previewKind(mimeType: string): MediaKind {
  return mediaKind(mimeType);
}

/** Only an image or a video expands, as on desktop. */
export function canOpenPreview(mimeType: string): boolean {
  const kind = previewKind(mimeType);
  return kind === "image" || kind === "video";
}

export function composerAttachmentTestID(index: number): string {
  return `composer-attachment-${index}`;
}
export function removeAttachmentTestID(index: number): string {
  return `btn-remove-attachment-${index}`;
}
export function sensitiveToggleTestID(index: number): string {
  return `btn-sensitive-attachment-${index}`;
}

export function canMarkSensitive(mimeType: string): boolean {
  return canOpenPreview(mimeType);
}

/** A marked card stays covered in the composer too. */
export function cardOpens(att: Pick<PickedAttachment, "mimeType" | "sensitive">): boolean {
  return canOpenPreview(att.mimeType) && att.sensitive !== true;
}

/** No object key yet: every consumer draws the picker's local file. */
export function pickedToMessageAttachment(
  att: PickedAttachment,
  uploadedAt: number,
): MessageAttachment {
  return {
    id: att.id,
    object_key: "",
    content_hash: "",
    sha256: "",
    key_scope: null,
    filename: att.name,
    content_type: att.mimeType,
    file_size: 0,
    uploaded_at: uploadedAt,
    width: att.width,
    height: att.height,
    blurhash: att.blurhash,
    sensitive: att.sensitive,
    localPreviewUri: att.uri,
  };
}

/** The strip's images and videos in strip order; the draft is one message. */
export function draftViewerItems(
  picked: readonly PickedAttachment[],
  attachmentId: string,
): ViewerItems {
  return collectViewerItems(
    [
      {
        id: "draft",
        created_at: 0,
        attachments: picked.map((att) => pickedToMessageAttachment(att, 0)),
      },
    ],
    attachmentId,
  );
}

// Handed over here: a pushed page takes string params only, and the URIs are private.
let draftForViewer: readonly PickedAttachment[] = [];

export function setDraftForViewer(picked: readonly PickedAttachment[]): void {
  draftForViewer = picked.slice();
}

export function getDraftForViewer(): readonly PickedAttachment[] {
  return draftForViewer;
}

export interface PickerAsset {
  assetId?: string | null;
  uri: string;
  fileName?: string | null;
  mimeType?: string | null;
  type?: string | null;
  width?: number;
  height?: number;
}

/** An untyped video gets `.mp4` / `video/mp4`, not the image fallback. */
export function pickedFromAssets(assets: readonly PickerAsset[], now: number): PickedAttachment[] {
  return assets.map((asset, i) => {
    const isVideo = asset.type === "video" || !!asset.mimeType?.startsWith("video/");
    return {
      id: asset.assetId ?? `${now}-${i}`,
      uri: asset.uri,
      name: asset.fileName ?? (isVideo ? `video-${now}-${i}.mp4` : `image-${now}-${i}.jpg`),
      mimeType: asset.mimeType ?? (isVideo ? "video/mp4" : "image/jpeg"),
      width: asset.width,
      height: asset.height,
    };
  });
}
