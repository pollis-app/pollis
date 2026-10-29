/* Back/forward enablement as a pure reducer: the DOM has no `canGoForward`, so track index and maxIndex. A PUSH resets the ceiling. */

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
  /** `__TSR_index`; optional since a foreign `pushState` leaves it unstamped. */
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

/** Where an unstamped entry landed, clamped to the known stack. */
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

/** Fold one history notification in; PUSH truncates the forward stack. */
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

/** Adopt an unwatched stack; the ceiling starts at the cursor. */
export function syncHistoryNav(index: number | undefined): HistoryNavState {
  const resolved = asIndex(index) ?? 0;
  return { index: resolved, maxIndex: resolved };
}
