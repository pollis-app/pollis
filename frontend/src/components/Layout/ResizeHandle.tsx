import React, { useRef } from "react";
import { clampPanelPx, resolveDrag } from "./panelWidth";

interface ResizeHandleProps {
  /** The panel this handle resizes. Measured and written to directly. */
  panelRef: React.RefObject<HTMLElement | null>;
  /** Which inline edge of the panel the handle sits on — its INNER edge. */
  edge: "start" | "end";
  /** Accessible name, e.g. "Resize sidebar". */
  label: string;
  /** The widest the panel may currently be, asked once per drag. */
  maxPx: () => number;
  /** The width the user settled on. Not called if the pointer never moved. */
  onCommit: (px: number) => void;
  /** The user dragged below `MIN_PANEL_PX` — collapse instead of shrinking. */
  onCollapse: () => void;
  /** Double-click: drop back to the `--side-w` design token. */
  onReset: () => void;
}

/**
 * The drag affordance on a side panel's inner edge (#985).
 *
 * Shared by the left `Sidebar` and the right `RightPanel` — one gesture, one
 * set of bounds, one place to fix a bug in either. Slack, Discord and VS Code
 * all ship the same three behaviours and this matches them: drag to resize,
 * drag past the minimum to collapse, double-click to reset.
 *
 * ## Why the drag does not go through React
 *
 * `pointermove` fires at the pointer's sampling rate — 60-1000 Hz. A `setState`
 * per move would re-render the whole panel subtree each time: for the sidebar
 * that is every group, channel, DM row and badge, all to move one edge by a
 * few pixels. So a move writes `style.width` straight onto the panel element
 * and React is not involved at all; the store (and `localStorage`) is written
 * ONCE, on release, which is the only moment a width is worth remembering.
 * The imperative style is cleared on release before the store write, so the
 * committed render — not this element's leftovers — owns the width from then
 * on.
 *
 * ## Why collapse lands on release, not mid-drag
 *
 * This handle is a CHILD of the panel it resizes. Collapsing mid-drag would
 * unmount the element holding the pointer capture and strand the gesture, so
 * dragging below the minimum pins the panel AT the minimum — the user sees it
 * stop — and the collapse happens when they let go.
 */
export const ResizeHandle: React.FC<ResizeHandleProps> = ({
  panelRef,
  edge,
  label,
  maxPx,
  onCommit,
  onCollapse,
  onReset,
}) => {
  // Per-drag scratch. A ref, not state, for the same reason as above: none of
  // it is rendered, and touching state here would cost a render per move.
  const drag = useRef<{
    /** Panel geometry frozen at pointerdown — the anchored edge does not move. */
    anchorLeft: boolean;
    left: number;
    right: number;
    max: number;
    requested: number;
    moved: boolean;
  } | null>(null);

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    const panel = panelRef.current;
    if (!panel || e.button !== 0) {
      return;
    }
    const rect = panel.getBoundingClientRect();
    const handleRect = e.currentTarget.getBoundingClientRect();
    const centre = handleRect.left + handleRect.width / 2;
    // Which PHYSICAL edge moves is a question about the current layout, not
    // about `edge`: under `dir="rtl"` the sidebar is drawn on the right and
    // grows leftwards. Measuring which edge the handle is nearer answers it
    // without the component having to know the reading direction at all.
    const anchorLeft = Math.abs(centre - rect.right) < Math.abs(centre - rect.left);
    drag.current = {
      anchorLeft,
      left: rect.left,
      right: rect.right,
      max: maxPx(),
      requested: rect.width,
      moved: false,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
    e.currentTarget.dataset.dragging = "true";
    // The pointer leaves the handle constantly during a drag; without these
    // the cursor flickers back to a caret and the drag selects sidebar text.
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    const panel = panelRef.current;
    if (!state || !panel) {
      return;
    }
    state.requested = state.anchorLeft
      ? e.clientX - state.left
      : state.right - e.clientX;
    state.moved = true;
    panel.style.width = `${clampPanelPx(state.requested, state.max)}px`;
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    if (!state) {
      return;
    }
    drag.current = null;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    delete e.currentTarget.dataset.dragging;
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    const panel = panelRef.current;
    if (panel) {
      panel.style.width = "";
    }
    // A press with no movement is a click, not a resize — committing here
    // would freeze a token-default panel at whatever pixel width it happened
    // to measure, including on the first half of a double-click.
    if (!state.moved) {
      return;
    }
    const outcome = resolveDrag(state.requested, state.max);
    if (outcome.kind === "collapse") {
      // Deliberately no width write: the panel keeps the width it had, so
      // reopening it with `mod+b` restores that rather than the minimum or a
      // zero. Collapsed is a separate piece of state from how wide it is.
      onCollapse();
      return;
    }
    onCommit(outcome.px);
  };

  // Straddles the panel's hairline border so half the hit area is over the
  // neighbour — the VS Code sash arrangement. The visible affordance is the
  // 1px line at the very edge, solid, appearing on hover and staying for the
  // duration of the drag (no glow; `bg-line-strong` is the same token the
  // app's active borders use). Both variants paint the SAME colour, so which
  // one Tailwind emits last cannot matter.
  const placement = edge === "end" ? "-end-1" : "-start-1";
  const linePlacement = edge === "end" ? "end-1" : "start-1";

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      title={label}
      data-testid={`resize-handle-${edge}`}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onDoubleClick={onReset}
      className={`group absolute inset-y-0 ${placement} z-10 w-2 cursor-col-resize touch-none`}
    >
      <span
        aria-hidden="true"
        className={`pointer-events-none absolute inset-y-0 ${linePlacement} w-px group-hover:bg-line-strong group-data-[dragging=true]:bg-line-strong`}
      />
    </div>
  );
};
