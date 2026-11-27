/* Back/forward enablement as a pure reducer: the DOM has no `canGoForward`, so track index and maxIndex. A PUSH resets the ceiling. */

export interface HistoryNavState {
  /** Index of the current entry within the history stack (0 = the first). */
  index: number;
  /** Highest index still reachable forward from `index`. Never below it. */
  maxIndex: number;
}

/** TanStack's navigation kinds, plus POP: a BACK that discards the entry it left (#2129). */
export type HistoryNavAction = "PUSH" | "REPLACE" | "BACK" | "FORWARD" | "GO" | "POP";

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
  if (action === "BACK" || action === "POP") {
    return Math.max(state.index - 1, 0);
  }
  if (action === "FORWARD") {
    return Math.min(state.index + 1, state.maxIndex);
  }
  return state.index;
}

/** Fold one history notification in; PUSH and POP truncate the forward stack. */
export function reduceHistoryNav(
  state: HistoryNavState,
  event: HistoryNavEvent,
): HistoryNavState {
  const index = asIndex(event.index) ?? inferIndex(state, event.action);

  if (event.action === "PUSH" || event.action === "POP") {
    return { index, maxIndex: index };
  }

  return { index, maxIndex: Math.max(state.maxIndex, index) };
}

/** Adopt an unwatched stack; the ceiling starts at the cursor. */
export function syncHistoryNav(index: number | undefined): HistoryNavState {
  const resolved = asIndex(index) ?? 0;
  return { index: resolved, maxIndex: resolved };
}

/** Each seen entry's pathname by stack index; PUSH and POP drop the entries ahead. */
export function reduceHistoryPaths(
  paths: readonly (string | undefined)[],
  action: HistoryNavAction,
  index: number,
  pathname: string,
): (string | undefined)[] {
  const next = action === "PUSH" || action === "POP" ? paths.slice(0, index) : paths.slice();
  next[index] = pathname;
  return next;
}

/**
 * How a finished create/edit page moves on to `target` (#2129). "pop" when the entry behind
 * it is already `target`: step back and drop the form, so neither Back nor Forward finds it
 * and the stack holds no twin of the page. Otherwise "replace": the form's entry becomes the
 * result, and Back goes where it went before the form opened.
 */
export function finishMove(
  paths: readonly (string | undefined)[],
  index: number,
  target: string,
): "pop" | "replace" {
  return index > 0 && paths[index - 1] === target ? "pop" : "replace";
}
