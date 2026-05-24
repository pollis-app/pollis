import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
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
  /** Floor cell width in px. Below this, the grid wraps to more rows. */
  minCellWidth?: number;
  /** Ceiling cell width in px. Stops one or two tiles from ballooning
   *  to fill the whole container — Discord-style "give each tile its
   *  fair share, but no more than this". */
  maxCellWidth?: number;
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
  maxW: number,
  aspect: number,
  gap: number,
): Layout {
  // Only W gates layout — cellH is derived from cellW via the aspect
  // ratio, so H is informational. Bailing on H <= 0 used to cause the
  // observer view (NavigableGrid in a flex column where height settles
  // after first paint) to render zero-sized tiles forever, since
  // box.h started at 0 and the H-gated branch returned cellW: 0 from
  // every recompute.
  void H;
  if (n === 0 || W <= 0) {
    return { cols: 1, cellW: 0, cellH: 0 };
  }
  // Discord-style sizing: each tile gets `W / n` of the row width,
  // clamped between minW and maxW. With one or two participants this
  // prevents tiles from ballooning to fill the whole container; with
  // many participants it prevents them from being squeezed to nothing
  // (the layout wraps to additional rows instead and the container
  // scrolls). Aspect-ratio is fixed (16:9 by default); height is
  // derived, not negotiated against the container's height.
  const target = Math.max(minW, Math.min(maxW, W / n));
  // How many tiles of `target` width (plus gaps between them) actually
  // fit in this row? When target is forced up to minW (many users),
  // this is < n and we wrap. When target is forced down to maxW (one
  // user), cols == 1.
  let cols = Math.max(1, Math.floor((W + gap) / (target + gap)));
  cols = Math.min(cols, n);
  const cellW = Math.min(target, W);
  return { cols, cellW, cellH: cellW / aspect };
}

export function NavigableGrid<T>({
  items,
  getKey,
  renderCell,
  onActivate,
  minCellWidth = 168,
  maxCellWidth = 500,
  aspect = 16 / 9,
  gap = 12,
  autoFocus = true,
  emptyLabel = "No one here yet.",
  testId,
}: NavigableGridProps<T>) {
  // Track the grid container via a state-backed ref callback rather than
  // useRef + useLayoutEffect with [] deps. The grid root is conditionally
  // rendered (empty-state vs items-state are different JSX subtrees), so
  // a single-shot useLayoutEffect binds ResizeObserver to whichever
  // element happens to be mounted at first paint — when the empty branch
  // renders first (query loading → 0 items → emptyLabel) and the items
  // branch renders a tick later (data arrives → N items), the ref-based
  // approach silently never installs the observer on the items
  // container, box.w stays 0, computeLayout returns cellW: 0, and tiles
  // render as zero-sized dots. The ref-callback variant re-fires the
  // effect whenever the actual DOM element changes.
  const [containerEl, setContainerEl] = useState<HTMLDivElement | null>(null);
  const [focused, setFocused] = useState(0);
  const [box, setBox] = useState({ w: 0, h: 0 });

  useLayoutEffect(() => {
    if (!containerEl) {
      return;
    }
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) {
        setBox({ w: r.width, h: r.height });
      }
    });
    ro.observe(containerEl);
    return () => ro.disconnect();
  }, [containerEl]);

  // Keep focus in range as the participant set changes shape.
  useEffect(() => {
    setFocused((f) => Math.min(f, Math.max(0, items.length - 1)));
  }, [items.length]);

  useEffect(() => {
    if (autoFocus && items.length > 0) {
      containerEl?.focus();
    }
  }, [autoFocus, items.length]);

  const { cols, cellW, cellH } = useMemo(
    () => computeLayout(box.w, box.h, items.length, minCellWidth, maxCellWidth, aspect, gap),
    [box.w, box.h, items.length, minCellWidth, maxCellWidth, aspect, gap],
  );

  // Scroll the focused cell into view when navigating a scrolling grid.
  useEffect(() => {
    const el = containerEl?.querySelector<HTMLElement>(
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
      ref={setContainerEl}
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
            containerEl?.focus();
          }}
        >
          {renderCell(item, { focused: i === focused })}
        </div>
      ))}
    </div>
  );
}
