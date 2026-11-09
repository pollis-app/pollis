/** The media spotlight's roll (#1788): mobile's `collectViewerItems` (`mobile/lib/media/viewer.ts`) over one message. */

interface RollAttachment {
  id: string;
  content_type: string;
}

export interface Roll<T> {
  items: T[];
  index: number;
}

/** Images and videos walk with their neighbours; audio and files open alone. */
export function isRollMedia(contentType: string): boolean {
  return contentType.startsWith("image/") || contentType.startsWith("video/");
}

/** The message's images and videos in attachment order, and where `opened` sits in them. */
export function spotlightRoll<T extends RollAttachment>(
  attachments: readonly T[] | undefined,
  opened: T,
): Roll<T> {
  if (!isRollMedia(opened.content_type)) {
    return { items: [opened], index: 0 };
  }
  const seen = new Set<string>();
  const items: T[] = [];
  for (const attachment of attachments ?? []) {
    if (seen.has(attachment.id) || !isRollMedia(attachment.content_type)) {
      continue;
    }
    seen.add(attachment.id);
    items.push(attachment);
  }
  const index = items.findIndex((a) => a.id === opened.id);
  if (index < 0) {
    return { items: [opened], index: 0 };
  }
  return { items, index };
}

/** One step through a roll of `length`, wrapping at both ends as mobile's pager does. */
export function stepRoll(index: number, delta: number, length: number): number {
  if (length < 2) {
    return index;
  }
  return (((index + delta) % length) + length) % length;
}
