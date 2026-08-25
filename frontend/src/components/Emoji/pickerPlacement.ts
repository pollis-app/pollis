// Where an anchored panel opens on the Y axis, decided from the space the
// trigger actually has rather than from a prop fixed at the call site.
//
// A caller states a PREFERENCE ("the composer sits at the bottom, so open
// upward"). That preference is right almost always and wrong exactly when the
// trigger is near the edge it opens toward — a channel scrolled so the composer
// rides high, a short window, a large font setting. The panel then renders off
// the top of the content region, which `AppShell` clips with `overflow: hidden`,
// so the missing part is not merely off-screen but unreachable.

/** Gap between trigger and panel — matches the `mb-1`/`mt-1` in the class. */
export const PANEL_GAP_PX = 4;

/**
 * Below this a clamped panel is not worth showing on that side, so the decision
 * falls through to whichever side is roomier even if neither truly fits. Two
 * emoji rows plus the search box and footer.
 */
export const MIN_PANEL_HEIGHT_PX = 180;

export interface PlacementInput {
  /** What the caller would like, absent any constraint. */
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
  /**
   * Pixel cap when the chosen side cannot seat the panel at its natural
   * height, else `undefined` — the panel keeps its own height and nothing is
   * imposed on it.
   */
  maxHeight?: number;
}

/**
 * Resolve which side to open on, and how tall the panel may be.
 *
 * The preference wins whenever it fits, so the common case is unchanged and
 * this cannot introduce flapping on a window that has room. Only when the
 * preferred side is too tight does the other side get considered, and only
 * when NEITHER fits does the panel get clamped — to the roomier side, which is
 * the most panel the viewport can show.
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

  // Neither seats it. Take the roomier side and clamp — but keep the
  // preference on a tie so a symmetric trigger does not flip about.
  const roomier = usable(other) > usable(preferred) ? other : preferred;
  const room = usable(roomier);
  return { placement: roomier, maxHeight: Math.max(MIN_PANEL_HEIGHT_PX, room) };
}

/**
 * The panel's natural height in real pixels.
 *
 * The panel is sized in `rem` (`h-[24rem]`), which tracks the user's font
 * setting, so a hardcoded 384 would be wrong for exactly the people most likely
 * to hit a clipped panel — those running a larger font, where the panel is
 * bigger and the space is the same.
 */
export function panelHeightPx(remHeight: number): number {
  const root =
    typeof document !== "undefined"
      ? parseFloat(getComputedStyle(document.documentElement).fontSize)
      : NaN;
  return remHeight * (Number.isFinite(root) && root > 0 ? root : 16);
}
