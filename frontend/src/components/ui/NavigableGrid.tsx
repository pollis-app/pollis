import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

// Reusable arrow-key-navigable 2D grid.
//
// Sizing: cells target a fixed aspect ratio and grow to best-fill the
// container (à la a video-call grid). They shrink as participant count
// rises only down to `minCellWidth`; past that the grid scrolls
// vertically and arrow navigation keeps the focused cell in view.
//
// Navigation: Arrow keys move in 2D (clamped at edges), Enter activates
// the focused cell. Mouse hover is handled by the cell itself — the grid
// only owns keyboard focus, so hover and keyboard selection are
// independent triggers.

export interface NavigableGridProps<T> {
  items: T[];
  getKey: (item: T) => string;
  renderCell: (item: T, state: { focused: boolean }) => React.ReactNode;
  onActivate?: (item: T) => void;
  /** Floor cell width in px before the grid starts scrolling. */
  minCellWidth?: number;
  /** width / height. Default 16/9. */
  aspect?: number;
  gap?: number;
  autoFocus?: boolean;
  emptyLabel?: string;
  testId?: string;
}

interface Layout {
  cols: number;
  cellW: number;
  cellH: number;
}

function computeLayout(
  W: number,
  H: number,
  n: number,
  minW: number,
  aspect: number,
  gap: number,
): Layout {
  if (n === 0 || W <= 0 || H <= 0) {
    return { cols: 1, cellW: 0, cellH: 0 };
  }
  // Pick the column count whose limiting dimension yields the largest
  // cell — the classic "fit N rectangles of fixed aspect into a box".
  let bestCols = 1;
  let bestSize = 0;
  for (let c = 1; c <= n; c++) {
    const rows = Math.ceil(n / c);
    const cwByW = (W - gap * (c - 1)) / c;
    const chByH = (H - gap * (rows - 1)) / rows;
    const cwByH = chByH * aspect;
    const size = Math.min(cwByW, cwByH);
    if (size > bestSize) {
      bestSize = size;
      bestCols = c;
    }
  }
  let cols = bestCols;
  let cellW = (W - gap * (cols - 1)) / cols;
  // Enforce the usable-size floor by dropping columns (which enlarges
  // cells); the resulting overflow is absorbed by vertical scroll.
  while (cols > 1 && cellW < minW) {
    cols -= 1;
    cellW = (W - gap * (cols - 1)) / cols;
  }
  if (cellW > W) {
    cellW = W;
  }
  return { cols, cellW, cellH: cellW / aspect };
}

export function NavigableGrid<T>({
  items,
  getKey,
  renderCell,
  onActivate,
  minCellWidth = 168,
  aspect = 16 / 9,
  gap = 12,
  autoFocus = true,
  emptyLabel = "No one here yet.",
  testId,
}: NavigableGridProps<T>) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [focused, setFocused] = useState(0);
  const [box, setBox] = useState({ w: 0, h: 0 });

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) {
      return;
    }
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) {
        setBox({ w: r.width, h: r.height });
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Keep focus in range as the participant set changes shape.
  useEffect(() => {
    setFocused((f) => Math.min(f, Math.max(0, items.length - 1)));
  }, [items.length]);

  useEffect(() => {
    if (autoFocus && items.length > 0) {
      containerRef.current?.focus();
    }
  }, [autoFocus, items.length]);

  const { cols, cellW, cellH } = useMemo(
    () => computeLayout(box.w, box.h, items.length, minCellWidth, aspect, gap),
    [box.w, box.h, items.length, minCellWidth, aspect, gap],
  );

  // Scroll the focused cell into view when navigating a scrolling grid.
  useEffect(() => {
    const el = containerRef.current?.querySelector<HTMLElement>(
      `[data-grid-index="${focused}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [focused, cols]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const n = items.length;
      if (n === 0) {
        return;
      }
      switch (e.key) {
        case "ArrowRight":
          e.preventDefault();
          setFocused((f) => Math.min(n - 1, f + 1));
          break;
        case "ArrowLeft":
          e.preventDefault();
          setFocused((f) => Math.max(0, f - 1));
          break;
        case "ArrowDown":
          e.preventDefault();
          setFocused((f) => Math.min(n - 1, f + cols));
          break;
        case "ArrowUp":
          e.preventDefault();
          setFocused((f) => Math.max(0, f - cols));
          break;
        case "Home":
          e.preventDefault();
          setFocused(0);
          break;
        case "End":
          e.preventDefault();
          setFocused(n - 1);
          break;
        case "Enter": {
          const item = items[focused];
          if (item && onActivate) {
            e.preventDefault();
            onActivate(item);
          }
          break;
        }
      }
    },
    [items, focused, cols, onActivate],
  );

  if (items.length === 0) {
    return (
      <div
        className="flex-1 flex items-center justify-center font-mono text-xs"
        style={{ color: "var(--c-text-dim)" }}
        data-testid={testId}
      >
        {emptyLabel}
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      data-testid={testId}
      className="flex-1 overflow-y-auto overflow-x-hidden outline-none"
      style={{
        display: "grid",
        gridTemplateColumns: `repeat(${cols}, ${cellW}px)`,
        gridAutoRows: `${cellH}px`,
        gap: `${gap}px`,
        justifyContent: "center",
        alignContent: "start",
        padding: `${gap}px`,
      }}
    >
      {items.map((item, i) => (
        <div
          key={getKey(item)}
          data-grid-index={i}
          onMouseDown={(e) => {
            // A press on an interactive control inside the cell (volume
            // slider, View Stream button, …) must behave natively —
            // calling preventDefault here would kill the range drag and
            // block the control from taking focus. Only a press on the
            // inert tile body re-homes keyboard focus to the grid so
            // arrow-nav keeps working after a click.
            const target = e.target as HTMLElement;
            if (target.closest('input, button, a, select, textarea, [role="slider"]')) {
              setFocused(i);
              return;
            }
            e.preventDefault();
            setFocused(i);
            containerRef.current?.focus();
          }}
        >
          {renderCell(item, { focused: i === focused })}
        </div>
      ))}
    </div>
  );
}
