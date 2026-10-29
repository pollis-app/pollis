// Pure rules behind the composer's pending-attachment previews (#1585):
// mobile's port of desktop's `AttachmentPreview` in ChatInput.tsx. No
// react-native or expo imports, so `tests/composer-attachments.test.ts` can
// pin them under plain node.

import type { MessageAttachment } from "../types";
import type { PickedAttachment } from "./attachments";
import { collectViewerItems, mediaKind, type MediaKind, type ViewerItems } from "./media/viewer.ts";

/**
 * Side of a preview card, in points. Desktop's composer preview is 80px and
 * #1568 takes it up by half, to 120, when the sensitive toggle joins remove
 * in the toolbar; the phone matches that. With the filename under it the
 * strip stays well under a third of the shortest phone screen.
 */
export const COMPOSER_PREVIEW_SIZE = 120;

/** What a card draws: the image, a video's poster frame, or a file card. */
export function previewKind(mimeType: string): MediaKind {
  return mediaKind(mimeType);
}

/**
 * Whether tapping the card opens the media viewer. As on desktop, only an
 * image or a video expands; a file card has nothing more to show before send.
 */
export function canOpenPreview(mimeType: string): boolean {
  const kind = previewKind(mimeType);
  return kind === "image" || kind === "video";
}

/** Test ids, by the card's position in the strip (0-based). */
export function composerAttachmentTestID(index: number): string {
  return `composer-attachment-${index}`;
}
export function removeAttachmentTestID(index: number): string {
  return `btn-remove-attachment-${index}`;
}

/**
 * A picked attachment as the viewer and the optimistic row read it: no
 * object key yet, so every consumer draws the picker's local file
 * (`localPreviewUri`) and never touches the media transport.
 */
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
    localPreviewUri: att.uri,
  };
}

/**
 * The viewer's pages for a tapped draft card: the strip's images and videos
 * in strip order, so a swipe walks what the strip shows. The draft is treated
 * as one message, which is what it becomes on send.
 */
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

// The draft the viewer was last opened on. A pushed page takes string params
// only, and a picked file's URI is long and private to this process, so the
// strip hands its attachments over here instead. One at a time is enough:
// only the screen on top can open the viewer.
let draftForViewer: readonly PickedAttachment[] = [];

export function setDraftForViewer(picked: readonly PickedAttachment[]): void {
  draftForViewer = picked.slice();
}

export function getDraftForViewer(): readonly PickedAttachment[] {
  return draftForViewer;
}

/** The subset of an `expo-image-picker` asset the composer reads. */
export interface PickerAsset {
  assetId?: string | null;
  uri: string;
  fileName?: string | null;
  mimeType?: string | null;
  type?: string | null;
  width?: number;
  height?: number;
}

/**
 * Picked library assets → pending attachments. A video the picker could not
 * name or type gets a `.mp4` name and `video/mp4`, not the image fallback, so
 * the strip draws it as a video and the recipient's player can open it.
 */
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
