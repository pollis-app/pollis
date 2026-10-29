/**
 * Pure geometry for the two draggable side panels (`panel-width.test.ts`).
 * Widths are device-local `localStorage`, not keyed by user id because they
 * must apply at first paint; see `.codesight/wiki/ui.md` → "Resizable side
 * panels".
 */

/**
 * Smallest width a panel may hold; dragging narrower collapses it. px, not rem:
 * a pointer-hit bound that shouldn't scale with font size.
 */
export const MIN_PANEL_PX = 50;

/**
 * Pixels the centre column always keeps. Each panel's max is reduced by the
 * width the other actually occupies.
 */
export const MIN_CONTENT_PX = 50;

export const SIDEBAR_WIDTH_KEY = "pollis.layout.sidebarWidthPx";
export const RIGHT_PANEL_WIDTH_KEY = "pollis.layout.rightPanelWidthPx";

export type DragOutcome = { kind: "collapse" } | { kind: "width"; px: number };

/**
 * Widest one panel may be while the other holds `otherPanelPx` (0 if closed).
 * Never below `MIN_PANEL_PX`: a tiny window must not collapse a panel.
 */
export function maxPanelPx(windowPx: number, otherPanelPx: number): number {
  return Math.max(MIN_PANEL_PX, Math.round(windowPx - otherPanelPx - MIN_CONTENT_PX));
}

/** Resolve a requested width; below the minimum is the collapse gesture, hence the union. */
export function resolveDrag(requestedPx: number, maxPx: number): DragOutcome {
  if (!Number.isFinite(requestedPx) || requestedPx < MIN_PANEL_PX) {
    return { kind: "collapse" };
  }
  return { kind: "width", px: Math.min(Math.round(requestedPx), maxPx) };
}

/**
 * Clamp a width into bounds, while dragging and when the window shrinks. Never
 * collapses: a window resize isn't aimed at this panel.
 */
export function clampPanelPx(px: number, maxPx: number): number {
  return Math.min(Math.max(Math.round(px), MIN_PANEL_PX), maxPx);
}

/** A stored width, or null (none / junk), meaning render at the `--side-w` token. */
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
