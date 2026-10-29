import React, { useRef } from "react";
import { clampPanelPx, resolveDrag } from "./panelWidth";

interface ResizeHandleProps {
  panelRef: React.RefObject<HTMLElement | null>;
  edge: "start" | "end";
  label: string;
  maxPx: () => number;
  onCommit: (px: number) => void;
  onCollapse: () => void;
  onReset: () => void;
}

/** Drag handle on a side panel's inner edge (#985). Moves write `style.width` directly (no React per move); the store is written once on release. Collapse lands on release: this is a child of the panel, so unmounting mid-drag strands the capture. */
export const ResizeHandle: React.FC<ResizeHandleProps> = ({
  panelRef,
  edge,
  label,
  maxPx,
  onCommit,
  onCollapse,
  onReset,
}) => {
  // A ref: nothing here is rendered.
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
    // Measure which physical edge is nearer, so RTL needs no special case.
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
    // Keep the cursor and avoid text selection while the pointer leaves the handle.
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
    // No movement = a click; committing would freeze a token-default width.
    if (!state.moved) {
      return;
    }
    const outcome = resolveDrag(state.requested, state.max);
    if (outcome.kind === "collapse") {
      // No width write: reopening restores the old width.
      onCollapse();
      return;
    }
    onCommit(outcome.px);
  };

  // Straddles the border (VS Code sash); a solid 1px line on hover and drag.
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
