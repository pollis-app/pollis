/*
 * Message-id ↔ live DOM row for the windowed log (#874). Rows outside the
 * window aren't in the document, so permalinks, arrow-key nav and reply quotes
 * must ASK via `revealRow`, the only way any of them touch the DOM. See
 * `.codesight/wiki/ui.md` → "The windowed log".
 */

import type { Virtualizer } from "@tanstack/react-virtual";

/** Row-height guesses in rem; only rows never yet measured use them. */
const TERMINAL_ROW_REM = 2;
const REFINED_ROW_REM = 3.25;

/** Rows mounted beyond each viewport edge, so one-step moves stay synchronous. */
export const ROW_OVERSCAN = 10;

export type MessageRowVirtualizer = Virtualizer<HTMLDivElement, Element>;

function rootFontSizePx(): number {
  const raw = parseFloat(
    getComputedStyle(document.documentElement).fontSize || "",
  );
  return Number.isFinite(raw) && raw > 0 ? raw : 16;
}

export function estimateRowPx(skin: string): number {
  const rem = skin === "refined" ? REFINED_ROW_REM : TERMINAL_ROW_REM;
  return rem * rootFontSizePx();
}

/** Distance from the top that triggers a load-more; rem so it scales (#934). */
const LOAD_MORE_THRESHOLD_REM = 10;

/** The load-more threshold in px, read per call so it follows a font-size change. */
export function loadMoreThresholdPx(): number {
  return LOAD_MORE_THRESHOLD_REM * rootFontSizePx();
}

/** The same from an explicit root size, for the unit test. */
export function loadMoreThresholdPxAt(rootPx: number): number {
  return LOAD_MORE_THRESHOLD_REM * rootPx;
}

/** What `revealRow` needs; the caller passes a ref-backed object so an in-flight reveal reads the current timeline. */
export interface MessageRowWindow {
  container: () => HTMLElement | null;
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

/** Frames to wait for a row after moving the window; the cap turns a never-rendering target into null. */
const MAX_REVEAL_FRAMES = 30;

function nextFrame(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

/**
 * Get a message's row into the live DOM and return it: immediately when
 * rendered, else after moving the window and awaiting the commit.
 * `isCancelled` is checked every frame so a re-run effect can abandon it.
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
