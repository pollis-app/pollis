import React, { useEffect, useRef } from "react";
import { clampPanelPx, resolveDrag } from "./panelWidth";
import "./ResizeHandle.css";

interface ResizeHandleProps {
  panelRef: React.RefObject<HTMLElement | null>;
  edge: "start" | "end";
  label: string;
  maxPx: () => number;
  onCommit: (px: number) => void;
  onCollapse: () => void;
  onReset: () => void;
}

/** Drag handle on a side panel's inner edge (#985). Moves write `style.width` directly (no React per move); the store is written once on release. Collapse lands on release: this is a child of the panel, so unmounting mid-drag strands the capture. A drag selects nothing (#2128): the press's default action is cancelled and `<html data-resizing>` turns selection off page-wide until the drag ends, however it ends. */
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
    handle: HTMLDivElement;
    pointerId: number;
    /** The blur listener as added, so the same function is removed whichever render ends the drag. */
    onBlur: () => void;
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
      handle: e.currentTarget,
      pointerId: e.pointerId,
      onBlur: endDrag,
    };
    // Cancels the press's default action, so no text selection starts from it.
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    e.currentTarget.dataset.dragging = "true";
    // Selection off and the cursor held page-wide while the pointer leaves the handle.
    document.documentElement.dataset.resizing = "true";
    window.addEventListener("blur", endDrag);
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

  /** Clears what a drag sets outside React: the handle's flag, the page-wide flag, the blur listener. */
  function clearDragChrome({ handle, onBlur }: { handle: HTMLDivElement; onBlur: () => void }) {
    window.removeEventListener("blur", onBlur);
    delete handle.dataset.dragging;
    delete document.documentElement.dataset.resizing;
  }

  /** Ends the drag on release, cancel, a lost capture or the window losing focus; a second call is a no-op. */
  function endDrag() {
    const state = drag.current;
    if (!state) {
      return;
    }
    drag.current = null;
    clearDragChrome(state);
    if (state.handle.hasPointerCapture(state.pointerId)) {
      state.handle.releasePointerCapture(state.pointerId);
    }
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
  }

  // Unmounted mid-drag (the panel closed by a shortcut): selection must not stay off.
  useEffect(() => {
    return () => {
      const state = drag.current;
      if (state) {
        drag.current = null;
        clearDragChrome(state);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
      onLostPointerCapture={endDrag}
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
