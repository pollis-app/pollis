/*
 * Back/forward enablement as a pure state machine. `history.length` can't
 * answer "forward?", so track `index` (TanStack's `__TSR_index`) and
 * `maxIndex`, the furthest reachable entry. A PUSH from mid-stack discards
 * everything ahead, so it RESETS the ceiling to the new entry.
 */

export interface HistoryNavState {
  index: number;
  /** Highest index reachable forward; never below `index`. */
  maxIndex: number;
}

export type HistoryNavAction = "PUSH" | "REPLACE" | "BACK" | "FORWARD" | "GO";

export interface HistoryNavEvent {
  action: HistoryNavAction;
  /**
   * `__TSR_index` of the entry navigated to. Optional: a foreign `pushState`
   * leaves it unstamped, and the reducer infers it from the action.
   */
  index?: number;
}

export const INITIAL_HISTORY_NAV: HistoryNavState = { index: 0, maxIndex: 0 };

export function canGoBack(state: HistoryNavState): boolean {
  return state.index > 0;
}

export function canGoForward(state: HistoryNavState): boolean {
  return state.index < state.maxIndex;
}

function asIndex(value: number | undefined): number | null {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    return null;
  }
  return value;
}

/** Where an unstamped entry landed, from the action alone, clamped to the known stack. */
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

/** Fold one history notification in: PUSH truncates; other actions move the cursor. */
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
 * Adopt a stack we haven't watched. The ceiling starts AT the cursor: what's
 * ahead is unknowable until the user actually goes back.
 */
export function syncHistoryNav(index: number | undefined): HistoryNavState {
  const resolved = asIndex(index) ?? 0;
  return { index: resolved, maxIndex: resolved };
}
