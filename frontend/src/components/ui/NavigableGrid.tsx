import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import { horizontalArrowStep } from "../../utils/direction";

// Arrow-key-navigable 2D grid. Cells keep a fixed aspect and best-fill the
// container down to `minCellWidth`, then the grid scrolls. The grid owns only
// keyboard focus; hover is the cell's. Left/right resolve against the computed
// direction (`horizontalArrowStep`), since CSS Grid mirrors under RTL.

export interface NavigableGridProps<T> {
  items: T[];
  getKey: (item: T) => string;
  renderCell: (item: T, state: { focused: boolean }) => React.ReactNode;
  onActivate?: (item: T) => void;
  /** Floor cell width in px; below it the grid wraps. */
  minCellWidth?: number;
  /** Ceiling cell width in px, so one or two tiles don't balloon. */
  maxCellWidth?: number;
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
  // Only W gates layout (H is derived via the aspect): gating on H rendered
  // zero-sized tiles forever where height settles after first paint.
  void H;
  if (n === 0 || W <= 0) {
    return { cols: 1, cellW: 0, cellH: 0 };
  }
  // Each tile gets `W / n`, clamped to [minW, maxW]; past the floor it wraps.
  const target = Math.max(minW, Math.min(maxW, W / n));
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
  emptyLabel,
  testId,
}: NavigableGridProps<T>) {
  const { t } = useTranslation("common");
  // Resolved at render so the fallback follows a language change.
  const resolvedEmptyLabel = emptyLabel ?? t("grid.empty");
  // A state-backed ref callback, not useRef + one-shot effect: the root is
  // conditionally rendered (empty vs items), and the observer must follow the
  // element actually mounted or tiles stay zero-sized.
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
      const step = horizontalArrowStep(e.key, containerEl);
      if (step !== 0) {
        e.preventDefault();
        setFocused((f) => Math.max(0, Math.min(n - 1, f + step)));
        return;
      }
      switch (e.key) {
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
    [items, focused, cols, onActivate, containerEl],
  );

  if (items.length === 0) {
    return (
      <div
        className="flex-1 flex items-center justify-center font-mono text-xs text-dim"
        data-testid={testId}
      >
        {resolvedEmptyLabel}
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
            // A press on a control inside the cell behaves natively; only the
            // inert tile body re-homes keyboard focus to the grid.
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
