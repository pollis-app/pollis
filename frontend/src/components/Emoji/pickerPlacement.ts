// Which side an anchored panel opens on, from the trigger's real space: a fixed side gets clipped (AppShell `overflow: hidden`).

/** Matches `mb-1`/`mt-1`. */
export const PANEL_GAP_PX = 4;

/** Below this a side isn't worth clamping into; take the roomier one. */
export const MIN_PANEL_HEIGHT_PX = 180;

export interface PlacementInput {
  preferred: "up" | "down";
  spaceAbove: number;
  spaceBelow: number;
  desired: number;
}

export interface Placement {
  placement: "up" | "down";
  /** Pixel cap when the side can't seat the natural height. */
  maxHeight?: number;
}

/** The preference wins when it fits; clamp to the roomier side only when neither does. */
export function resolvePlacement({
  preferred,
  spaceAbove,
  spaceBelow,
  desired,
}: PlacementInput): Placement {
  const usableAbove = Math.max(0, spaceAbove - PANEL_GAP_PX);
  const usableBelow = Math.max(0, spaceBelow - PANEL_GAP_PX);
  const other = preferred === "up" ? "down" : "up";
  const usable = (side: "up" | "down") => (side === "up" ? usableAbove : usableBelow);

  if (usable(preferred) >= desired) {
    return { placement: preferred };
  }
  if (usable(other) >= desired) {
    return { placement: other };
  }

  // Neither fits: roomier side, preference on a tie.
  const roomier = usable(other) > usable(preferred) ? other : preferred;
  const room = usable(roomier);
  return { placement: roomier, maxHeight: Math.max(MIN_PANEL_HEIGHT_PX, room) };
}

/** Natural height in px; rem-sized, so it grows with the font setting. */
export function panelHeightPx(remHeight: number): number {
  const root =
    typeof document !== "undefined"
      ? parseFloat(getComputedStyle(document.documentElement).fontSize)
      : NaN;
  return remHeight * (Number.isFinite(root) && root > 0 ? root : 16);
}
