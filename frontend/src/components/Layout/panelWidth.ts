/** Pure geometry for the two side panels (tested in panel-width.test.ts). Widths are device-local `localStorage`, not synced, and not keyed by user: they must be on the element at first paint. */

/** Below this a drag collapses. px: a mouse-target bound, not font-scaled. */
export const MIN_PANEL_PX = 50;

/** The centre column's floor; each panel's max subtracts the other's actual width. */
export const MIN_CONTENT_PX = 50;

export const SIDEBAR_WIDTH_KEY = "pollis.layout.sidebarWidthPx";
export const RIGHT_PANEL_WIDTH_KEY = "pollis.layout.rightPanelWidthPx";

export type DragOutcome = { kind: "collapse" } | { kind: "width"; px: number };

/** Widest allowed for one panel; never below `MIN_PANEL_PX`. */
export function maxPanelPx(windowPx: number, otherPanelPx: number): number {
  return Math.max(MIN_PANEL_PX, Math.round(windowPx - otherPanelPx - MIN_CONTENT_PX));
}

/** Below the minimum is the collapse gesture, hence the union. */
export function resolveDrag(requestedPx: number, maxPx: number): DragOutcome {
  if (!Number.isFinite(requestedPx) || requestedPx < MIN_PANEL_PX) {
    return { kind: "collapse" };
  }
  return { kind: "width", px: Math.min(Math.round(requestedPx), maxPx) };
}

/** Clamp a committed width; never collapses. */
export function clampPanelPx(px: number, maxPx: number): number {
  return Math.min(Math.max(Math.round(px), MIN_PANEL_PX), maxPx);
}

/** Null = no answer yet: render at the `--side-w` token. */
export function parseStoredWidth(raw: string | null): number | null {
  if (raw === null) {
    return null;
  }
  const px = Number(raw);
  if (!Number.isFinite(px) || px < MIN_PANEL_PX) {
    return null;
  }
  return Math.round(px);
}
