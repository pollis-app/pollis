// Which way an anchored panel opens, from the trigger's actual space. The
// caller's preference fails near an edge (high composer, short window, large
// font), and `AppShell` clips overflow, making the panel unreachable.

/** Gap between trigger and panel — matches the `mb-1`/`mt-1` in the class. */
export const PANEL_GAP_PX = 4;

/** Below this (two rows + search + footer) a side isn't worth clamping to. */
export const MIN_PANEL_HEIGHT_PX = 180;

export interface PlacementInput {
  preferred: "up" | "down";
  /** Viewport pixels above the trigger's top edge. */
  spaceAbove: number;
  /** Viewport pixels below the trigger's bottom edge. */
  spaceBelow: number;
  /** The panel's natural height in pixels. */
  desired: number;
}

export interface Placement {
  placement: "up" | "down";
    /** Pixel cap when the chosen side can't seat the panel, else `undefined`. */
  maxHeight?: number;
}

/**
 * Resolve the side and max height. The preference wins whenever it fits (no
 * flapping); only when neither side fits is the panel clamped to the roomier.
 */
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

  // Neither fits: roomier side, keeping the preference on a tie.
  const roomier = usable(other) > usable(preferred) ? other : preferred;
  const room = usable(roomier);
  return { placement: roomier, maxHeight: Math.max(MIN_PANEL_HEIGHT_PX, room) };
}

/** The panel's natural height in px; it is sized in `rem`, so it grows with the font setting. */
export function panelHeightPx(remHeight: number): number {
  const root =
    typeof document !== "undefined"
      ? parseFloat(getComputedStyle(document.documentElement).fontSize)
      : NaN;
  return remHeight * (Number.isFinite(root) && root > 0 ? root : 16);
}
