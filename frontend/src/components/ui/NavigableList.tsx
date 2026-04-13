import React, { useState, useEffect, useCallback, useRef } from "react";

// Reusable keyboard-navigable list.
//
// Navigation model:
//   colIndex 0         → the row itself (Enter fires onEnterRow)
//   colIndex 1..N      → one of the right-aligned `controls`
//
// Keys: ArrowUp/Down move between rows (and reset colIndex to 0),
// ArrowLeft/Right move between the row and its controls, Enter either
// fires onEnterRow (when on the row) or is handled natively by the
// focused control (e.g. Button/Switch translate Enter to click).
export interface NavigableListProps<T> {
  items: T[];

  // Unique, stable React key per item.
  getKey: (item: T) => string;

  // Main row content — typically the name + subtitle/preview.
  renderRow: (item: T) => React.ReactNode;

  // Right-aligned focusable controls. Return an empty array (or omit) for
  // a row with no controls. Each element must contain a focusable child
  // (button, [role="switch"], input, etc.).
  controls?: (item: T) => React.ReactNode[];

  // Right-aligned non-focusable trailing content (timestamps, labels).
  trailing?: (item: T) => React.ReactNode;

  // Called when Enter is pressed while focus is on the row (colIndex 0).
  onEnterRow?: (item: T) => void;

  isLoading?: boolean;
  loadingLabel?: string;
  emptyLabel?: string;

  rowTestId?: (item: T) => string;
  testId?: string;
}

type NavState = { rowIndex: number; colIndex: number };

export function NavigableList<T>({
  items,
  getKey,
  renderRow,
  controls,
  trailing,
  onEnterRow,
  isLoading = false,
  loadingLabel = "Loading…",
  emptyLabel = "No items.",
  rowTestId,
  testId,
}: NavigableListProps<T>) {
  const [nav, setNav] = useState<NavState>({ rowIndex: 0, colIndex: 0 });
  const containerRef = useRef<HTMLDivElement>(null);

  // Reset navigation when the item set changes shape.
  useEffect(() => {
    setNav({ rowIndex: 0, colIndex: 0 });
  }, [items.length]);

  // Move DOM focus to match nav state.
  useEffect(() => {
    if (items.length === 0) {
      return;
    }
    const item = items[nav.rowIndex];
    if (!item) {
      return;
    }
    if (nav.colIndex === 0) {
      containerRef.current?.focus();
      return;
    }
    const rowEl = containerRef.current?.querySelector<HTMLElement>(
      `[data-nav-row-key="${getKey(item)}"]`,
    );
    if (!rowEl) {
      return;
    }
    const controlWrapper = rowEl.querySelector<HTMLElement>(
      `[data-nav-control-index="${nav.colIndex - 1}"]`,
    );
    if (!controlWrapper) {
      return;
    }
    const focusable = controlWrapper.querySelector<HTMLElement>(
      'button, [role="switch"], input, [tabindex]:not([tabindex="-1"])',
    );
    focusable?.focus();
  }, [nav, items, getKey]);

  // Take initial focus when there's something to navigate.
  useEffect(() => {
    if (!isLoading && items.length > 0) {
      containerRef.current?.focus();
    }
  }, [isLoading, items.length]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (items.length === 0) {
        return;
      }
      const item = items[nav.rowIndex];
      const rowControlCount = item && controls ? controls(item).length : 0;
      const maxCol = rowControlCount;

      switch (e.key) {
        case "ArrowUp": {
          e.preventDefault();
          setNav((prev) => ({
            rowIndex: prev.rowIndex > 0 ? prev.rowIndex - 1 : items.length - 1,
            colIndex: 0,
          }));
          break;
        }
        case "ArrowDown": {
          e.preventDefault();
          setNav((prev) => ({
            rowIndex: prev.rowIndex < items.length - 1 ? prev.rowIndex + 1 : 0,
            colIndex: 0,
          }));
          break;
        }
        case "ArrowRight": {
          if (maxCol === 0) {
            break;
          }
          e.preventDefault();
          setNav((prev) => ({
            ...prev,
            colIndex: prev.colIndex < maxCol ? prev.colIndex + 1 : maxCol,
          }));
          break;
        }
        case "ArrowLeft": {
          if (maxCol === 0) {
            break;
          }
          e.preventDefault();
          setNav((prev) => ({
            ...prev,
            colIndex: prev.colIndex > 0 ? prev.colIndex - 1 : 0,
          }));
          break;
        }
        case "Enter": {
          if (nav.colIndex === 0) {
            if (!onEnterRow || !item) {
              break;
            }
            e.preventDefault();
            onEnterRow(item);
          }
          break;
        }
      }
    },
    [items, nav, controls, onEnterRow],
  );

  if (isLoading) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          {loadingLabel}
        </p>
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
          {emptyLabel}
        </p>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      data-testid={testId}
      className="flex-1 flex flex-col overflow-auto outline-none"
    >
      {items.map((item, rowIndex) => {
        const key = getKey(item);
        const isRowFocused = nav.rowIndex === rowIndex;
        const rowControls = controls?.(item) ?? [];

        return (
          <div
            key={key}
            data-nav-row-key={key}
            data-testid={rowTestId?.(item)}
            className="flex items-center px-4 py-2 gap-3 text-xs font-mono select-none"
            style={{
              background: isRowFocused ? "var(--c-active)" : undefined,
              borderLeft: isRowFocused
                ? "2px solid var(--c-accent)"
                : "2px solid transparent",
            }}
          >
            {/* Row cursor indicator */}
            <span
              className="w-3 flex-shrink-0 text-center"
              style={{ color: "var(--c-accent)" }}
            >
              {isRowFocused && nav.colIndex === 0 ? ">" : " "}
            </span>

            {/* Main row content */}
            <div className="flex-1 min-w-0 flex items-center gap-3">
              {renderRow(item)}
            </div>

            {/* Focusable controls */}
            {rowControls.length > 0 && (
              <div className="flex items-center gap-8 flex-shrink-0">
                {rowControls.map((control, i) => (
                  <div key={i} data-nav-control-index={i}>
                    {control}
                  </div>
                ))}
              </div>
            )}

            {/* Non-focusable trailing */}
            {trailing && (
              <div
                className="flex-shrink-0 text-xs font-mono"
                style={{ color: "var(--c-text-muted)" }}
              >
                {trailing(item)}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
