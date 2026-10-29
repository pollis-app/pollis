/**
 * Chat-log keyboard navigation (bash-history style) as a pure state machine:
 * ArrowUp from the composer walks up the log, Left/Right walk a row's action
 * bar, ArrowDown past newest (or Tab/Escape) returns. One union, changed only by
 * `transitionMessageNav`, testable without a DOM (`message-nav.test.ts`). The
 * rendered action bar (via the context) is the source of truth for actions.
 */

export type MessageNavAction = "reply" | "react" | "edit" | "more";

export type MessageNavState =
  | { mode: "composer" }
  | { mode: "message"; messageId: string }
  | { mode: "action"; messageId: string; action: MessageNavAction };

export type MessageNavEvent =
  | { type: "enter" }
  /** One older; holds at the oldest. */
  | { type: "up" }
  /** One newer; past the newest exits to the composer. */
  | { type: "down" }
  /** Toward the bar's end; from the bare row selects the first action. */
  | { type: "nextAction" }
  /** Back along the bar; from the first action returns to the bare row. */
  | { type: "prevAction" }
  /** Deliberate exit (Tab / Escape); the caller refocuses the composer. */
  | { type: "exit" }
  /** Focus left some other way; reset, but never steal focus back. */
  | { type: "cancel" };

/**
 * `ids` is the navigable list oldest → newest. Returns the SAME state object on
 * no-op transitions so identity-keyed effects don't refire.
 */
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
  // The focused message vanished: re-anchor to the newest.
  if (index === -1) {
    const newest = ids[ids.length - 1];
    return newest === undefined
      ? { mode: "composer" }
      : { mode: "message", messageId: newest };
  }

  switch (event.type) {
    case "up": {
      if (index === 0) {
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
          // No actions on this row (tombstone).
        return state;
      }
      if (state.mode === "message") {
        return { mode: "action", messageId: state.messageId, action: first };
      }
      const at = actions.indexOf(state.action);
      if (at === -1) {
          // The selected action is gone: clamp to the first.
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
