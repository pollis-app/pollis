import { useCallback, useEffect, useMemo, useRef } from "react";
import { panelWidthStore, type PanelId } from "../../stores/panelWidthStore";

/**
 * Everything a resizable side panel needs to be one (#985).
 *
 * Both panels wire the same three things — a ref on the `<aside>`, the width
 * to draw it at, and the props for its `ResizeHandle` — so they are derived
 * here once rather than copied into `Sidebar` and `RightPanel`.
 *
 * Must be called from an `observer()` component: the width read below is what
 * repaints the panel after a drag commits or a window resize clamps it.
 */
export function usePanelWidth(id: PanelId, onCollapse: () => void) {
  const ref = useRef<HTMLElement | null>(null);
  const width = panelWidthStore.widthOf(id);

  // Report the rendered width back to the store after every render, and 0 on
  // unmount. This is the only way the OTHER panel can know how much room it
  // has left when this one is still on the `--side-w` token: that is a rem
  // measure, so only the DOM has it in pixels. No dependency array on purpose
  // — a re-render is exactly when the number can have changed (skin, font
  // size, open/closed) — and it is one `offsetWidth` read into a non-
  // observable field, so it cannot loop.
  useEffect(() => {
    const el = ref.current;
    if (!el) {
      return;
    }
    panelWidthStore.reportMeasured(id, el.offsetWidth);
    // Re-clamp on the same beat. A sidebar may legitimately have been dragged
    // very wide while the right panel was CLOSED — nothing was competing with
    // it then — so the moment the other panel appears the pair has to be
    // pulled back inside the window. Converges: the clamp only ever narrows,
    // and a pass that changes nothing writes nothing, so no render loop.
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
    // `w-side` only while this device has no answer, so the default keeps
    // tracking the skin's rem token; a dragged width is a runtime-computed
    // value and belongs in an inline style. Never both — two sources for one
    // CSS property is exactly the ambiguity the class-order rule forbids.
    widthClass: width === null ? "w-side" : "",
    widthStyle: width === null ? undefined : { width },
    handleProps,
  };
}
