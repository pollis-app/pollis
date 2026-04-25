import React, { useState, useRef, useEffect, useCallback } from "react";
import { ChevronRight, ArrowUp, ArrowDown, MoreVertical } from "lucide-react";
import { Button } from "./Button";

export interface TerminalMenuItem {
  id: string;
  label: string;
  description?: React.ReactNode;
  action?: () => void;
  disabled?: boolean;
  icon?: React.ReactNode;
  // "separator" renders a horizontal rule; "system" dims the item (for nav/action items vs content items)
  type?: "separator" | "system";
  testId?: string;
  // Unread count badge — shown next to label when > 0
  badge?: number;
  // Secondary action rendered as a ⋮ button on the right of the row
  secondaryAction?: () => void;
  secondaryActionLabel?: string;
}

interface TerminalMenuProps {
  items: TerminalMenuItem[];
  onEsc?: () => void;
  className?: string;
  autoFocus?: boolean;
}

export const TerminalMenu: React.FC<TerminalMenuProps> = ({
  items,
  onEsc,
  className = "",
  autoFocus = true,
}) => {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [isFocused, setIsFocused] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLDivElement | null)[]>([]);

  useEffect(() => {
    itemRefs.current = itemRefs.current.slice(0, items.length);
  }, [items.length]);

  useEffect(() => {
    if (autoFocus) {
      containerRef.current?.focus();
    }
  }, [autoFocus]);

  // Reset selection to first navigable item only when the actual item IDs change,
  // not on every parent re-render that produces a new array reference.
  const itemIds = items.map((i) => i.id).join(",");
  useEffect(() => {
    const first = items.findIndex((item) => item.type !== "separator");
    setSelectedIndex(first >= 0 ? first : 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [itemIds]);

  // Skip separators when navigating
  const navigableIndices = items
    .map((item, i) => ({ item, i }))
    .filter(({ item }) => item.type !== "separator")
    .map(({ i }) => i);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case "ArrowUp": {
          e.preventDefault();
          const pos = navigableIndices.indexOf(selectedIndex);
          const prevPos = pos > 0 ? pos - 1 : navigableIndices.length - 1;
          setSelectedIndex(navigableIndices[prevPos]);
          break;
        }
        case "ArrowDown": {
          e.preventDefault();
          const pos = navigableIndices.indexOf(selectedIndex);
          const nextPos = pos < navigableIndices.length - 1 ? pos + 1 : 0;
          setSelectedIndex(navigableIndices[nextPos]);
          break;
        }
        case "Enter":
        case " ": {
          e.preventDefault();
          const item = items[selectedIndex];
          if (item && !item.disabled) {
            item.action?.();
          }
          break;
        }
        case "Escape":
          e.preventDefault();
          e.stopPropagation();
          onEsc?.();
          break;
        case "Home":
          e.preventDefault();
          setSelectedIndex(0);
          break;
        case "End":
          e.preventDefault();
          setSelectedIndex(items.length - 1);
          break;
      }
    },
    [items, selectedIndex, onEsc, navigableIndices]
  );

  useEffect(() => {
    itemRefs.current[selectedIndex]?.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }, [selectedIndex]);

  const handleItemClick = useCallback((item: TerminalMenuItem, index: number) => {
    setSelectedIndex(index);
    if (!item.disabled) {
      item.action?.();
    }
  }, []);

  return (
    <div
      ref={containerRef}
      className={`outline-none ${className}`}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      onFocus={() => setIsFocused(true)}
      onBlur={() => setIsFocused(false)}
      onMouseDown={(e) => {
        // Keep keyboard focus on the menu when clicking anywhere in it, including dead zones below the last item
        if (document.activeElement !== containerRef.current) {
          e.preventDefault();
          containerRef.current?.focus();
        }
      }}
      role="menu"
      aria-label="Navigation menu"
    >
      {/* Keyboard hints */}
      <div
        className="flex items-center gap-1 px-4 py-2 text-xs font-mono flex-shrink-0"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <ArrowUp className="w-3 h-3" />
        <ArrowDown className="w-3 h-3" />
        <span>navigate</span>
        <span className="mx-1" style={{ color: "var(--c-border-active)" }}>•</span>
        <span>Enter to select</span>
        {onEsc && (
          <>
            <span className="mx-1" style={{ color: "var(--c-border-active)" }}>•</span>
            <span>Esc to go back</span>
          </>
        )}
      </div>

      {/* Items */}
      <div className="overflow-y-auto">
        {items.map((item, index) => {
          if (item.type === "separator") {
            return (
              <div
                key={item.id}
                style={{ borderTop: "2px solid var(--c-border)", margin: "0" }}
                aria-hidden="true"
              />
            );
          }

          const isSelected = index === selectedIndex;
          const isSystem = item.type === "system";

          return (
            <div
              key={item.id}
              ref={(el) => { itemRefs.current[index] = el; }}
              data-testid={item.testId}
              className="flex items-center gap-3 px-4 py-3 cursor-pointer transition-colors"
              style={{
                borderLeft: `3px solid ${isSelected ? "var(--c-accent)" : "transparent"}`,
                background: isSelected ? "var(--c-active)" : undefined,
                opacity: item.disabled ? 0.4 : 1,
                cursor: item.disabled ? "not-allowed" : "pointer",
              }}
              onClick={() => handleItemClick(item, index)}
              onMouseEnter={() => { if (!item.disabled) { setSelectedIndex(index); } }}
              role="menuitem"
              aria-disabled={item.disabled}
            >
              {/* Chevron indicator — stays centered in the full row height */}
              <div className="w-4 h-4 flex-shrink-0 flex items-center justify-center">
                {isSelected
                  ? <ChevronRight className="w-4 h-4" style={{ color: "var(--c-accent)" }} />
                  : <div className="w-4 h-4" />
                }
              </div>

              <div className="flex-1 min-w-0">
                <div
                  className="font-sans text-sm flex items-center gap-3"
                  style={{
                    color: isSelected
                      ? "var(--c-accent)"
                      : isSystem
                        ? "var(--c-text-muted)"
                        : "var(--c-text)",
                  }}
                >
                  {item.icon && (
                    <span className="flex-shrink-0" style={{ color: isSelected ? "var(--c-accent)" : "var(--c-text-dim)" }}>
                      {item.icon}
                    </span>
                  )}
                  <span>{item.label}</span>
                  {item.badge != null && item.badge > 0 && (
                    <span
                      className="font-sans text-xs"
                      style={{ color: "var(--c-accent)" }}
                    >
                      [{item.badge}]
                    </span>
                  )}
                </div>
                {item.description && (
                  <div
                    className="text-xs font-sans mt-0.5 truncate"
                    style={{ color: "var(--c-text-muted)" }}
                  >
                    {item.description}
                  </div>
                )}
              </div>

              {item.secondaryAction && (
                <Button
                  data-testid={item.testId ? `${item.testId}-secondary` : undefined}
                  aria-label={item.secondaryActionLabel ?? "More options"}
                  variant="secondary"
                  className="flex-shrink-0 !px-1 !py-0.5"
                  onClick={(e) => {
                    e.stopPropagation();
                    item.secondaryAction!();
                  }}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      item.secondaryAction!();
                    }
                  }}
                >
                  <MoreVertical size={14} aria-hidden="true" />
                </Button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
};
