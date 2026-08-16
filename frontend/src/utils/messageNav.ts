/**
 * Chat-log keyboard navigation (bash-history style) — the pure state machine.
 *
 * ArrowUp from an empty composer (or with the caret on the first line) leaves
 * the input and walks up the message log; each focused row exposes its hover
 * action bar to Left/Right; ArrowDown past the newest message (or Tab/Escape)
 * returns to the composer. REPL muscle memory, end to end.
 *
 * The whole space of focus positions is one discriminated union, and the ONLY
 * way to move through it is `transitionMessageNav` — so contradictory states
 * ("an action is selected but no message is", "navigating while the composer
 * owns focus") are unrepresentable, and every transition is a total function
 * over (state × event) that can be unit-tested without a DOM
 * (`frontend/tests/message-nav.test.ts`).
 *
 * Deliberately free of MobX and the DOM: the store (`stores/messageNavStore`)
 * owns the live state, and `MessageList` projects it onto real element focus.
 * The rendered action bar is the source of truth for which actions a message
 * has — the context callback reads it — so this module never re-derives
 * per-message permissions and cannot drift from what is actually on screen.
 */

/** The composer-bar actions a focused message can expose, in visual order. */
export type MessageNavAction = "reply" | "edit" | "more";

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
  /** ArrowDown within the log — one message newer; past the newest exits
   *  back to the composer. */
  | { type: "down" }
  /** One step along the action bar, toward its end. From the bare row this
   *  selects the first action. */
  | { type: "nextAction" }
  /** One step back along the action bar. From the first action this returns
   *  to the bare row. */
  | { type: "prevAction" }
  /** Deliberate exit (Tab / Escape) — the caller refocuses the composer. */
  | { type: "exit" }
  /** Focus left the log some other way (a click elsewhere, the surface
   *  changed). State resets but the caller must NOT steal focus back. */
  | { type: "cancel" };

/**
 * Transition function. `ids` is the rendered, navigable message list oldest →
 * newest; `actionsFor` reports the action bar of one message in visual order.
 *
 * Returns the SAME state object when nothing changes, so effect hooks keyed
 * on identity don't refire on no-op transitions (e.g. ArrowUp at the oldest
 * message).
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

  // Movement events only exist inside the log.
  if (state.mode === "composer") {
    return state;
  }

  const index = ids.indexOf(state.messageId);
  // The focused message vanished (deleted, filtered, conversation refetched):
  // re-anchor to the newest message rather than dead-ending.
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
