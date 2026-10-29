// Touch feedback on a message row (#1282): timings and surfaces behind
// components/chat/MessageRow.tsx, free of react-native for the node tests. The
// fade waits a beat, so a flick that scrolls the list never flashes rows.

export const PRESS_FEEDBACK = {
  // Android's tap timeout (ViewConfiguration.getTapTimeout), the delay its
  // own lists use before showing a pressed state.
  delayMs: 100,
  fadeInMs: 120,
  fadeOutMs: 180,
} as const;

// Shared by the row and its attachments, so both open the sheet together.
export const LONG_PRESS_MS = 350;

// Tinted when it mentions the reader or was just jumped to.
export function restingSurface(tinted: boolean): "bg" | "accentFaint" {
  return tinted ? "accentFaint" : "bg";
}

// One step up from where it rests, so the press reads on a tinted row too.
export function pressedSurface(tinted: boolean): "raised" | "accentSoft" {
  return tinted ? "accentSoft" : "raised";
}
