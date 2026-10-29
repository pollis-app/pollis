import { useCallback, useEffect, useMemo, useRef } from "react";
import { panelWidthStore, type PanelId } from "../../stores/panelWidthStore";

/**
 * Ref, width and `ResizeHandle` props for a resizable side panel (#985). Call
 * from an `observer()`: the width read repaints after a commit or clamp.
 */
export function usePanelWidth(id: PanelId, onCollapse: () => void) {
  const ref = useRef<HTMLElement | null>(null);
  const width = panelWidthStore.widthOf(id);

  // Report the rendered width (0 on unmount) after every render: only the DOM
  // knows the `--side-w` token in px, and the other panel needs it. No deps on
  // purpose; it writes a non-observable field, so it can't loop.
  useEffect(() => {
    const el = ref.current;
    if (!el) {
      return;
    }
    panelWidthStore.reportMeasured(id, el.offsetWidth);
    // Re-clamp too: a sidebar dragged wide while the other panel was closed
    // must shrink when it opens. Only narrows, so it converges.
    panelWidthStore.clampToWindow(window.innerWidth);
    return () => panelWidthStore.reportMeasured(id, 0);
  });

  const maxPx = useCallback(() => panelWidthStore.maxFor(id, window.innerWidth), [id]);
  const onCommit = useCallback((px: number) => panelWidthStore.setWidth(id, px), [id]);
  const onReset = useCallback(() => panelWidthStore.setWidth(id, null), [id]);

  const handleProps = useMemo(
    () => ({ panelRef: ref, maxPx, onCommit, onCollapse, onReset }),
    [maxPx, onCommit, onCollapse, onReset],
  );

  return {
    ref,
      // `w-side` only while there's no stored width; never both class and style.
    widthClass: width === null ? "w-side" : "",
    widthStyle: width === null ? undefined : { width },
    handleProps,
  };
}
