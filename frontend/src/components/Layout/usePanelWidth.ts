import { useCallback, useEffect, useMemo, useRef } from "react";
import { panelWidthStore, type PanelId } from "../../stores/panelWidthStore";

/** Ref, width and handle props for a resizable panel (#985). Call from an `observer()`. */
export function usePanelWidth(id: PanelId, onCollapse: () => void) {
  const ref = useRef<HTMLElement | null>(null);
  const width = panelWidthStore.widthOf(id);

  // Report the rendered width (0 on unmount) every render: only the DOM knows `--side-w` in px. Can't loop.
  useEffect(() => {
    const el = ref.current;
    if (!el) {
      return;
    }
    panelWidthStore.reportMeasured(id, el.offsetWidth);
  // Re-clamp too; converges since it only narrows.
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
    // `w-side` only with no stored width; never both a class and an inline width.
    widthClass: width === null ? "w-side" : "",
    widthStyle: width === null ? undefined : { width },
    handleProps,
  };
}
