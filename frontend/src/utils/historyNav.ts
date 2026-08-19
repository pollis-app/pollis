/*
 * Browser-style back/forward enablement — the pure state machine.
 *
 * WHY THIS EXISTS
 * ---------------
 * "Can I go back?" is answerable from any history implementation; "can I go
 * FORWARD?" is not. The DOM exposes no `canGoForward`, and `history.length` is
 * useless for it — it counts the whole session's entries, not the ones ahead of
 * the cursor, and it never shrinks when a push truncates the forward stack. So
 * the only way to grey out a forward chevron honestly is to watch navigation as
 * it happens and keep the two numbers that answer both questions:
 *
 *   index    — where the cursor is, straight off the history entry's own
 *              `__TSR_index` (TanStack stamps it into the entry state)
 *   maxIndex — the furthest entry still reachable ahead of it
 *
 * `canGoBack` is then `index > 0` and `canGoForward` is `index < maxIndex`.
 *
 * The one rule that is easy to get wrong: a PUSH from the middle of the stack
 * DISCARDS everything ahead of it (that is what a browser does, and what
 * TanStack's memory history literally does with `entries.splice`). So a push
 * does not raise the ceiling, it RESETS it to the new entry — otherwise the
 * forward chevron would stay lit pointing at entries that no longer exist.
 *
 * Kept pure and out of the store so all of that is testable without a router,
 * a DOM or MobX — same split as `messageNav.ts`.
 */

/** The cursor and the ceiling. Nothing else is needed to answer either question. */
export interface HistoryNavState {
  /** Index of the current entry within the history stack (0 = the first). */
  index: number;
  /** Highest index still reachable forward from `index`. Never below it. */
  maxIndex: number;
}

/** The navigation kinds TanStack's history reports to its subscribers. */
export type HistoryNavAction = "PUSH" | "REPLACE" | "BACK" | "FORWARD" | "GO";

/** A history notification: what happened, and the index it landed on. */
export interface HistoryNavEvent {
  action: HistoryNavAction;
  /**
   * `__TSR_index` of the entry navigated to. Optional because it is read out
   * of an entry's state, which a foreign `pushState` (an extension, a stray
   * script) can leave unstamped — the reducer then derives it from the action
   * rather than throwing away the whole stack.
   */
  index?: number;
}

/** A fresh session: sitting on the first entry, nothing ahead. */
export const INITIAL_HISTORY_NAV: HistoryNavState = { index: 0, maxIndex: 0 };

/** Whether a real entry exists behind the cursor. */
export function canGoBack(state: HistoryNavState): boolean {
  return state.index > 0;
}

/** Whether a real entry exists ahead of the cursor. */
export function canGoForward(state: HistoryNavState): boolean {
  return state.index < state.maxIndex;
}

/** A usable stack index, or null when the value is missing or nonsensical. */
function asIndex(value: number | undefined): number | null {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    return null;
  }
  return value;
}

/**
 * Where an unstamped entry must have landed, judged from the action alone.
 *
 * Deliberately clamped to the stack we already know about: a BACK from the
 * first entry stays on it, and a FORWARD past the ceiling cannot invent an
 * entry to move onto.
 */
function inferIndex(state: HistoryNavState, action: HistoryNavAction): number {
  if (action === "PUSH") {
    return state.index + 1;
  }
  if (action === "BACK") {
    return Math.max(state.index - 1, 0);
  }
  if (action === "FORWARD") {
    return Math.min(state.index + 1, state.maxIndex);
  }
  return state.index;
}

/**
 * Folds one history notification into the cursor/ceiling pair.
 *
 * A PUSH truncates the forward stack (see the header); every other action just
 * moves the cursor, so the ceiling only ever rises to accommodate an index we
 * did not know about yet.
 */
export function reduceHistoryNav(
  state: HistoryNavState,
  event: HistoryNavEvent,
): HistoryNavState {
  const index = asIndex(event.index) ?? inferIndex(state, event.action);

  if (event.action === "PUSH") {
    return { index, maxIndex: index };
  }

  return { index, maxIndex: Math.max(state.maxIndex, index) };
}

/**
 * Adopts a history stack we have not been watching — on attach, or after a
 * router is swapped out from under us.
 *
 * The ceiling starts AT the cursor rather than above it: entries ahead of an
 * index we have never seen a navigation for are unknowable, and "forward is
 * dark until you have actually gone back" is the honest reading of that.
 */
export function syncHistoryNav(index: number | undefined): HistoryNavState {
  const resolved = asIndex(index) ?? 0;
  return { index: resolved, maxIndex: resolved };
}
