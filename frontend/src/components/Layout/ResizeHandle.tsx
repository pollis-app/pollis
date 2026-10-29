import React, { useRef } from "react";
import { clampPanelPx, resolveDrag } from "./panelWidth";

interface ResizeHandleProps {
  /** Measured and written to directly. */
  panelRef: React.RefObject<HTMLElement | null>;
  /** The panel's INNER inline edge. */
  edge: "start" | "end";
  label: string;
  /** Asked once per drag. */
  maxPx: () => number;
  /** Not called if the pointer never moved. */
  onCommit: (px: number) => void;
  onCollapse: () => void;
  onReset: () => void;
}

/**
 * Drag affordance on a side panel's inner edge (#985), shared by `Sidebar` and
 * `RightPanel`: drag to resize, past the minimum to collapse, double-click to
 * reset.
 *
 * Moves write `style.width` directly (a `setState` per pointermove would
 * re-render the whole panel); the store is written once on release, after the
 * imperative style is cleared. Collapse lands on release because this handle
 * is a child of the panel and unmounting mid-drag would strand the capture.
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
  // A ref, not state: nothing here is rendered.
  const drag = useRef<{
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
    // Which PHYSICAL edge moves depends on layout (under RTL the sidebar grows
    // leftwards), so measure which edge the handle is nearer.
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
    // Without these the cursor flickers and the drag selects sidebar text.
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
    // No movement is a click: committing would freeze a token-default panel
    // at its measured px, including on a double-click's first half.
    if (!state.moved) {
      return;
    }
    const outcome = resolveDrag(state.requested, state.max);
    if (outcome.kind === "collapse") {
      // No width write: reopening restores the width it had.
      onCollapse();
      return;
    }
    onCommit(outcome.px);
  };

  // Straddles the hairline border (VS Code sash). Both variants paint the same
  // colour, so Tailwind's emit order can't matter.
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
