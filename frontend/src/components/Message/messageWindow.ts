/*
 * Windowing the message log (#874).
 *
 * The log renders a bounded window of rows instead of the whole history, so
 * layout and paint stop scaling with how far back a conversation goes. The
 * windowing itself is `@tanstack/react-virtual`; this module is the part that
 * is specific to us — the bridge between "a message id" and "a live DOM row",
 * which three subsystems depend on and which windowing is what breaks.
 *
 * `messageJumpStore` (permalinks), `messageNavStore` (arrow-key focus) and the
 * reply-quote scroll all locate a row with `querySelector`. A row outside the
 * window is not in the document at all, so they cannot simply look: they have
 * to ASK for it first and wait for the render. `revealRow` is that ask, and it
 * is deliberately the only way any of them touch the DOM.
 */

import type { Virtualizer } from "@tanstack/react-virtual";

/**
 * Row-height guesses, in rem so the estimate tracks the user's font setting
 * (a px constant would put a 20px-root user's window in the wrong place).
 * Only unmeasured rows use these — every row that has been on screen once is
 * remembered at its real height, keyed by timeline key, so the estimate stops
 * mattering as soon as the user has been there.
 */
const TERMINAL_ROW_REM = 2;
const REFINED_ROW_REM = 3.25;

/**
 * Rows kept mounted beyond each edge of the viewport. Generous on purpose:
 * the cost of an off-screen row is one row, and the benefit is that ordinary
 * one-step interactions (ArrowUp off the top edge, a reply quote a few rows
 * up) find their target already rendered and stay synchronous.
 */
export const ROW_OVERSCAN = 10;

export type MessageRowVirtualizer = Virtualizer<HTMLDivElement, Element>;

/** Root font size in px, i.e. what 1rem currently means. */
function rootFontSizePx(): number {
  const raw = parseFloat(
    getComputedStyle(document.documentElement).fontSize || "",
  );
  return Number.isFinite(raw) && raw > 0 ? raw : 16;
}

/** Starting height guess for an unmeasured row of the given skin. */
export function estimateRowPx(skin: string): number {
  const rem = skin === "refined" ? REFINED_ROW_REM : TERMINAL_ROW_REM;
  return rem * rootFontSizePx();
}

/**
 * What `revealRow` needs to know about the list. Deliberately three functions
 * rather than a component reference: the caller passes a ref-backed object, so
 * a reveal in flight always reads the CURRENT timeline instead of the one that
 * existed when the effect fired.
 */
export interface MessageRowWindow {
  /** The scroll container, or null before it mounts. */
  container: () => HTMLElement | null;
  /** Timeline index of a message id, or undefined when the log has no such row. */
  indexOf: (messageId: string) => number | undefined;
  virtualizer: MessageRowVirtualizer;
}

/** The rendered row for a message id, or null when it is outside the window. */
export function findRow(
  win: MessageRowWindow,
  messageId: string,
): HTMLElement | null {
  return (
    win.container()?.querySelector<HTMLElement>(
      `[data-testid="message-${messageId}"]`,
    ) ?? null
  );
}

/**
 * Frames to wait for a row to appear after asking the window to move.
 *
 * The range recomputes on the scroll offset change, so in practice this
 * resolves on the first or second frame; the ceiling only exists so a target
 * that never renders (filtered out mid-flight, container detached) fails as a
 * null rather than as a hung promise.
 */
const MAX_REVEAL_FRAMES = 30;

function nextFrame(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

/**
 * Get a message's row into the live DOM and hand it back.
 *
 * Returns synchronously-resolved when the row is already rendered, which is
 * the common case and keeps ordinary navigation free. Otherwise it moves the
 * window to the row's index and waits for React to commit.
 *
 * `isCancelled` is checked after every frame so an effect that re-runs (a new
 * jump request, a conversation switch) can abandon an in-flight reveal instead
 * of scrolling somewhere the user has already left.
 */
export async function revealRow(
  win: MessageRowWindow,
  messageId: string,
  align: "auto" | "start" | "center" | "end",
  isCancelled: () => boolean,
): Promise<HTMLElement | null> {
  const alreadyThere = findRow(win, messageId);
  if (alreadyThere) {
    return alreadyThere;
  }
  const index = win.indexOf(messageId);
  if (index === undefined) {
    return null;
  }
  win.virtualizer.scrollToIndex(index, { align });
  for (let frame = 0; frame < MAX_REVEAL_FRAMES; frame++) {
    await nextFrame();
    if (isCancelled()) {
      return null;
    }
    const row = findRow(win, messageId);
    if (row) {
      return row;
    }
  }
  return null;
}
