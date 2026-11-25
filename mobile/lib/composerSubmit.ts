// The composer's send gate (#2099): one submit per draft, and the field stays clear after it.
//
// Two ways one tap could send twice, both from the native text field racing JS:
// - Taps that queue up while the JS thread is busy are delivered in one batch, before React
//   renders the cleared composer, so every tap's handler read the same draft and sent it.
// - The native field drops a clear from JS while it holds a text change JS has not read yet
//   (React Native's `mostRecentEventCount` check). That change then reaches JS after the
//   send and put the sent text back in the composer, Send enabled, ready to go again.
// The gate takes the text at the tap, refuses a second tap until the composer has rendered
// again, and ignores those late reports, so the controlled field clears the native text.

/** How long after a send a native report can still be one the field sent before it. */
export const ECHO_WINDOW_MS = 300;

export type SubmitGate = {
  /** False from a submit until the composer next commits a render. */
  armed: boolean;
  /** The newest text the field reported and the composer took, read at the tap. */
  latest: string;
  /** The last submit that cleared the field, while a late report of it may still come. */
  sent: { text: string; at: number } | null;
};

export function newSubmitGate(draft: string): SubmitGate {
  return { armed: true, latest: draft, sent: null };
}

/** After every commit: the composer shows `draft`, and a tap may submit again. */
export function rearm(gate: SubmitGate, draft: string): void {
  gate.armed = true;
  gate.latest = draft;
}

/**
 * The text a tap submits, or null when it must do nothing: nothing to send, or a submit
 * already taken since the composer last rendered. Disarms the gate when it returns text.
 */
export function takeSubmit(gate: SubmitGate, canSendEmptyText: boolean): string | null {
  if (!gate.armed) {
    return null;
  }
  const text = gate.latest;
  if (!text.trim() && !canSendEmptyText) {
    return null;
  }
  gate.armed = false;
  return text;
}

/** The submit of `text` cleared the field: until the window ends, its late reports are stale. */
export function markCleared(gate: SubmitGate, text: string, now: number): void {
  gate.latest = "";
  gate.sent = { text, at: now };
}

/**
 * Whether the field's report of `next` is to be taken into the draft.
 *
 * Refused: within the window after a clearing send, the sent text itself or anything longer
 * than one character. That is the field's pre-send text (maybe revised by autocorrect), not
 * new typing: nobody types two characters, pastes or picks a suggestion that fast. The
 * first report taken closes the window.
 */
export function acceptChange(gate: SubmitGate, next: string, now: number): boolean {
  const sent = gate.sent;
  if (sent && next !== "" && now - sent.at <= ECHO_WINDOW_MS) {
    if (next === sent.text || Array.from(next).length > 1) {
      return false;
    }
  }
  gate.sent = null;
  gate.latest = next;
  return true;
}
