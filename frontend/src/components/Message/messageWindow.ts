/* Windowing the message log (#874); `revealRow` is the only way callers get an off-window row into the DOM. */

import type { Virtualizer } from "@tanstack/react-virtual";

/** Row-height guesses in rem; only unmeasured rows use them. */
const TERMINAL_ROW_REM = 2;
const REFINED_ROW_REM = 3.25;

/** Rows kept mounted beyond each edge, so one-step moves stay synchronous. */
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

/** Top-of-log load-more distance, in rem (#934). */
const LOAD_MORE_THRESHOLD_REM = 10;

/** Load-more threshold in px at the current root size; read per call. */
export function loadMoreThresholdPx(): number {
  return LOAD_MORE_THRESHOLD_REM * rootFontSizePx();
}

/** Same, from an explicit root size (for the unit test). */
export function loadMoreThresholdPxAt(rootPx: number): number {
  return LOAD_MORE_THRESHOLD_REM * rootPx;
}

/** Ref-backed by the caller, so a reveal in flight reads the current timeline. */
export interface MessageRowWindow {
  container: () => HTMLElement | null;
  indexOf: (messageId: string) => number | undefined;
  virtualizer: MessageRowVirtualizer;
}

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

/** Frame cap so a row that never renders resolves null, not a hung promise. */
const MAX_REVEAL_FRAMES = 30;

function nextFrame(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

/** Get a message's row into the DOM, moving the window if needed; `isCancelled` aborts. */
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
