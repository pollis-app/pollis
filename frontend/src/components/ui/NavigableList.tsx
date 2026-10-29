import React, { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";

import { horizontalArrowStep, isHorizontalArrow } from "../../utils/direction";
import { EmptyState } from "./EmptyState";
import { usePressActivator, type ActivateOn } from "../../hooks/usePressActivation";

// Keyboard-navigable list. colIndex 0 is the row (Enter fires onEnterRow);
// 1..N are the inline-end `controls` (Enter handled natively). Left/right
// resolve against the computed direction (`utils/direction.ts`).
export interface NavigableListProps<T> {
  items: T[];

  getKey: (item: T) => string;

  renderRow: (item: T) => React.ReactNode;

  // Inline-end controls, each containing a focusable child.
  controls?: (item: T) => React.ReactNode[];

  // Non-focusable trailing content (timestamps, labels).
  trailing?: (item: T) => React.ReactNode;

  onEnterRow?: (item: T) => void;

  // Mouse click on the row body (not its controls). Separate from
  // `onEnterRow`, which some consumers wire to a destructive action.
  onClickRow?: (item: T) => void;
  // `"press"`: `onClickRow` runs on a plain mouse pointer-down (#1529) — only
  // when it navigates (free to undo). Enter and assistive tech still act on
  // their own path; a mouse press runs it once.
  activateRowOn?: ActivateOn;

  isLoading?: boolean;
  loadingLabel?: string;
  emptyLabel?: string;

  rowTestId?: (item: T) => string;
  testId?: string;

  // Grab focus when items first appear. Pass `false` when another element
  // wants initial focus.
  autoFocus?: boolean;

  // Show the cursor only while focused, for a second list on the page (#1506).
  cursorOnlyWhenFocused?: boolean;
}

type NavState = { rowIndex: number; colIndex: number };

export function NavigableList<T>({
  items,
  getKey,
  renderRow,
  controls,
  trailing,
  onEnterRow,
  onClickRow,
  activateRowOn,
  isLoading = false,
  loadingLabel,
  emptyLabel,
  rowTestId,
  testId,
  autoFocus = true,
  cursorOnlyWhenFocused = false,
}: NavigableListProps<T>) {
  const { t } = useTranslation("common");
  // Resolved at render so the fallbacks follow a language change.
  const resolvedLoadingLabel = loadingLabel ?? t("states.loading");
  const resolvedEmptyLabel = emptyLabel ?? t("list.empty");
  const [nav, setNav] = useState<NavState>({ rowIndex: 0, colIndex: 0 });
  const press = usePressActivator();
  const containerRef = useRef<HTMLDivElement>(null);
  const [hasFocusWithin, setHasFocusWithin] = useState(false);
  const showCursor = !cursorOnlyWhenFocused || hasFocusWithin;
  // Until the user navigates, don't move focus: it would clobber whatever the
  // parent focused.
  const hasNavigatedRef = useRef(false);

  useEffect(() => {
    setNav({ rowIndex: 0, colIndex: 0 });
  }, [items.length]);

  useEffect(() => {
    if (items.length === 0) {
      return;
    }
    const item = items[nav.rowIndex];
    if (!item) {
      return;
    }
    if (nav.colIndex === 0) {
      // Only after the user navigated: this re-runs on every parent render
      // (fresh `items` / `getKey`), and focusing the container scrolls the page
      // to it.
      if (hasNavigatedRef.current) {
        containerRef.current?.focus();
      }
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

  useEffect(() => {
    if (!autoFocus) {
      return;
    }
    if (!isLoading && items.length > 0) {
      containerRef.current?.focus();
    }
  }, [isLoading, items.length, autoFocus]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (items.length === 0) {
        return;
      }
      const item = items[nav.rowIndex];
      const rowControlCount = item && controls ? controls(item).length : 0;
      const maxCol = rowControlCount;

      // Now actively navigated, so focus-following may grab focus.
      if (
        e.key === "ArrowUp" ||
        e.key === "ArrowDown" ||
        isHorizontalArrow(e.key)
      ) {
        hasNavigatedRef.current = true;
      }

        // Resolved per event so a language switch needs no remount.
      const step = horizontalArrowStep(e.key, containerRef.current);
      if (step !== 0) {
        if (maxCol === 0) {
          return;
        }
        e.preventDefault();
        setNav((prev) => ({
          ...prev,
          colIndex: Math.max(0, Math.min(maxCol, prev.colIndex + step)),
        }));
        return;
      }

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
      <EmptyState background={false}>{resolvedLoadingLabel}</EmptyState>
    );
  }

  if (items.length === 0) {
    return (
      <div className="flex-1" style={{ paddingTop: "1rem", paddingInline: "1rem" }}>
        <p className="text-xs font-mono text-start text-dim">
          {resolvedEmptyLabel}
        </p>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      onFocus={cursorOnlyWhenFocused ? () => setHasFocusWithin(true) : undefined}
      onBlur={
        cursorOnlyWhenFocused
          ? (e) => {
              if (!containerRef.current?.contains(e.relatedTarget as Node | null)) {
                setHasFocusWithin(false);
              }
            }
          : undefined
      }
      data-testid={testId}
      className="flex-1 flex flex-col overflow-auto outline-none"
    >
      {items.map((item, rowIndex) => {
        const key = getKey(item);
        const isRowFocused = showCursor && nav.rowIndex === rowIndex;
        const rowControls = controls?.(item) ?? [];

        return (
          <div
            key={key}
            data-nav-row-key={key}
            data-testid={rowTestId?.(item)}
            className={`flex items-center px-4 py-2 gap-3 text-xs font-mono select-none border-s-2 ${isRowFocused ? "bg-active border-accent" : "border-transparent"}`}
          >
            <span className="w-3 flex-shrink-0 text-center text-accent">
              {isRowFocused && nav.colIndex === 0 ? ">" : " "}
            </span>

            <div
              className="flex-1 min-w-0 flex items-center gap-3"
              style={{ cursor: onClickRow ? "pointer" : undefined }}
              {...press<HTMLDivElement>(onClickRow ? () => onClickRow(item) : undefined, activateRowOn)}
            >
              {renderRow(item)}
            </div>

            {rowControls.length > 0 && (
              <div className="flex items-center gap-8 flex-shrink-0">
                {rowControls.map((control, i) => (
                  <div key={i} data-nav-control-index={i}>
                    {control}
                  </div>
                ))}
              </div>
            )}

            {trailing && (
              <div className="flex-shrink-0 text-xs font-mono text-muted">
                {trailing(item)}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
