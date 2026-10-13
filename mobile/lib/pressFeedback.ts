// Touch feedback on a message row (#1282): the timings and the surfaces
// behind components/chat/MessageRow.tsx, kept free of react-native so the
// node tests can pin them.
//
// Discord's and Slack's pattern: a finger resting on a message fades the
// row to a slightly raised surface, holds it through the long press that
// opens the message sheet, and clears it on release. The fade waits a beat
// before starting, so a flick that scrolls the list (which takes the touch
// within a few frames) never flashes the rows it passes over.

export const PRESS_FEEDBACK = {
  // How long a touch rests before the row starts to fade. Android's tap
  // timeout (ViewConfiguration.getTapTimeout) is 100 ms, the delay its own
  // lists use before showing a pressed state.
  delayMs: 100,
  // Fade in once the delay has passed.
  fadeInMs: 120,
  // Fade back out on release or cancel.
  fadeOutMs: 180,
} as const;

// How long a press must last to open the message sheet. The row and its
// attachments share it, so both open the sheet at the same moment.
export const LONG_PRESS_MS = 350;

// The surface a row rests on: the accent tint when it mentions the reader
// or was just jumped to, the background otherwise.
export function restingSurface(tinted: boolean): "bg" | "accentFaint" {
  return tinted ? "accentFaint" : "bg";
}

// The surface a pressed row fades to: one step up from where it rests, so
// the press reads on a tinted row as well as a plain one.
export function pressedSurface(tinted: boolean): "raised" | "accentSoft" {
  return tinted ? "accentSoft" : "raised";
}
