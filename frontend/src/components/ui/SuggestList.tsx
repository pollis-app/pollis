import React, { useEffect, useRef } from "react";
import { Trans } from "react-i18next";

interface SuggestListProps<T> {
  items: T[];
  activeIndex: number;
  query: string;
  testId: string;
  label: string;
  /** `common` key of the header line; its `hl` tag wraps the query. */
  matchingKey: string;
  itemKey: (item: T) => string;
  optionTestId: (item: T) => string;
  renderItem: (item: T) => React.ReactNode;
  onSelect: (item: T) => void;
  onHover: (index: number) => void;
}

/** Refined skin's composer autocomplete shell, `absolute` inside the composer (no portal); ChatInput owns the keys. */
export function SuggestList<T>({
  items,
  activeIndex,
  query,
  testId,
  label,
  matchingKey,
  itemKey,
  optionTestId,
  renderItem,
  onSelect,
  onHover,
}: SuggestListProps<T>) {
  const activeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  if (items.length === 0) {
    return null;
  }

  return (
    <div
      data-testid={testId}
      role="listbox"
      aria-label={label}
      className="absolute bottom-full left-0 right-0 z-20 mb-1 mx-2 overflow-hidden rounded-panel border border-line bg-surface-raised"
    >
      <div className="px-3 py-1.5 border-b border-line text-2xs text-muted select-none">
        {/* `Trans`, so a translator can move the highlighted query. */}
        <Trans
          i18nKey={matchingKey}
          ns="common"
          values={{ query }}
          components={{ hl: <span className="text-fg" /> }}
        />
      </div>
      <div className="max-h-60 overflow-y-auto py-1">
        {items.map((item, i) => {
          const isActive = i === activeIndex;
          return (
            <button
              key={itemKey(item)}
              ref={isActive ? activeRef : undefined}
              type="button"
              role="option"
              aria-selected={isActive}
              data-testid={optionTestId(item)}
              data-active={isActive ? "true" : undefined}
              // Keep composer focus: a blur tears down the list first.
              onMouseDown={(e) => {
                e.preventDefault();
                onSelect(item);
              }}
              onMouseEnter={() => onHover(i)}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-start text-sm hover:bg-hover ${
                isActive ? "bg-active" : ""
              }`}
            >
              {renderItem(item)}
            </button>
          );
        })}
      </div>
    </div>
  );
}
