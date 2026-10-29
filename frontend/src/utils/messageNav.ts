/** Bash-history-style chat-log keyboard nav as a pure union + total transition (tests: message-nav.test.ts). */

/** The composer-bar actions a focused message can expose, in visual order. */
export type MessageNavAction = "reply" | "react" | "edit" | "more";

export type MessageNavState =
  /** The chat input owns focus; the log is not being navigated. */
  | { mode: "composer" }
  /** A message row is focused (hover-style highlight), no action selected. */
  | { mode: "message"; messageId: string }
  /** A specific action button on a focused message row is selected. */
  | { mode: "action"; messageId: string; action: MessageNavAction };

export type MessageNavEvent =
  /** ArrowUp handed off from the composer — focus the newest message. */
  | { type: "enter" }
  /** ArrowUp within the log — one message older (stays at the oldest). */
  | { type: "up" }
  /** ArrowDown; past the newest exits to the composer. */
  | { type: "down" }
  /** Toward the action bar's end; from the bare row, the first action. */
  | { type: "nextAction" }
  /** Back along the bar; from the first action, the bare row. */
  | { type: "prevAction" }
  /** Deliberate exit (Tab / Escape) — the caller refocuses the composer. */
  | { type: "exit" }
  /** Focus left some other way; reset without stealing focus. */
  | { type: "cancel" };

/** `ids` oldest → newest. Returns the SAME state on no-ops so identity-keyed effects don't refire. */
export function transitionMessageNav(
  state: MessageNavState,
  event: MessageNavEvent,
  ids: readonly string[],
  actionsFor: (messageId: string) => readonly MessageNavAction[],
): MessageNavState {
  switch (event.type) {
    case "exit":
    case "cancel":
      return state.mode === "composer" ? state : { mode: "composer" };
    case "enter": {
      const newest = ids[ids.length - 1];
      if (newest === undefined) {
        return state.mode === "composer" ? state : { mode: "composer" };
      }
      return { mode: "message", messageId: newest };
    }
  }

  if (state.mode === "composer") {
    return state;
  }

  const index = ids.indexOf(state.messageId);
  // Focused message vanished: re-anchor to the newest.
  if (index === -1) {
    const newest = ids[ids.length - 1];
    return newest === undefined
      ? { mode: "composer" }
      : { mode: "message", messageId: newest };
  }

  switch (event.type) {
    case "up": {
      if (index === 0) {
        // Already at the oldest — drop any action selection, else hold still.
        return state.mode === "message" ? state : { mode: "message", messageId: state.messageId };
      }
      return { mode: "message", messageId: ids[index - 1] };
    }
    case "down": {
      if (index === ids.length - 1) {
        return { mode: "composer" };
      }
      return { mode: "message", messageId: ids[index + 1] };
    }
    case "nextAction": {
      const actions = actionsFor(state.messageId);
      const first = actions[0];
      if (first === undefined) {
        // No actions on this row (deleted tombstone) — nothing to select.
        return state;
      }
      if (state.mode === "message") {
        return { mode: "action", messageId: state.messageId, action: first };
      }
      const at = actions.indexOf(state.action);
      if (at === -1) {
        // The selected action no longer exists on this row — clamp to first.
        return { mode: "action", messageId: state.messageId, action: first };
      }
      const next = actions[at + 1];
      return next === undefined
        ? state
        : { mode: "action", messageId: state.messageId, action: next };
    }
    case "prevAction": {
      if (state.mode === "message") {
        return state;
      }
      const actions = actionsFor(state.messageId);
      const at = actions.indexOf(state.action);
      if (at <= 0) {
        return { mode: "message", messageId: state.messageId };
      }
      return { mode: "action", messageId: state.messageId, action: actions[at - 1] };
    }
  }
}
