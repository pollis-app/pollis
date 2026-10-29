// Touch feedback on a message row (#1282), pure. The fade waits a beat so a scroll never flashes rows.

export const PRESS_FEEDBACK = {
  // Android's tap timeout, the delay its own lists use.
  delayMs: 100,
  fadeInMs: 120,
  fadeOutMs: 180,
} as const;

// Shared by the row and its attachments.
export const LONG_PRESS_MS = 350;

export function restingSurface(tinted: boolean): "bg" | "accentFaint" {
  return tinted ? "accentFaint" : "bg";
}

// One step up from where it rests, so it reads on a tinted row too.
export function pressedSurface(tinted: boolean): "raised" | "accentSoft" {
  return tinted ? "accentSoft" : "raised";
}
