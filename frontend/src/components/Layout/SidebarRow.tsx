import React from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import type { ActivateOn } from "../../hooks/usePressActivation";
import { PressButton } from "../ui/PressButton";

// Sized in rem to track the font preference; the rem `size-[…]` class beats the SVG attribute.
export const sidebarIconProps = {
  size: 14,
  className: "size-[0.933rem] shrink-0",
} as const;

// Logical `ps-` indent, so RTL nests from the right.
export const indentPadClass = (indent: number): string =>
  indent >= 2 ? "ps-[2.8rem]" : "ps-[1.733rem]";

interface RowChevron {
  isCollapsed: boolean;
  onToggle: () => void;
  ariaLabel: string;
}

/** Where a drag would land relative to this row (#2031): a line above or below it. */
export type DropEdge = "before" | "after" | null;

export interface SidebarRowProps {
  indent: number;
  isActive: boolean;
  onClick: () => void;
  leading?: React.ReactNode;
  chevron?: RowChevron;
  label: string;
  badge?: number | null;
  trailing?: React.ReactNode;
  /** `data-testid` (`sidebar-row-*`, #932), separate from the translated label (#855). */
  testId?: string;
  /** A muted channel (#1431). */
  dimmed?: boolean;
  /** `"press"`: pointer-down (#1529). */
  activateOn?: ActivateOn;
  /** Hover or keyboard focus: the user is about to act on this row (voice warmup, #176). */
  onIntent?: () => void;
  /** Controls beside the row's own button (an admin's + and ⋯), shown on hover or focus. */
  actions?: React.ReactNode;
  /** Keep `actions` visible (their menu is open). */
  actionsPinned?: boolean;
  /** Drag-and-drop wiring for an admin's row (#2031); absent for everyone else. */
  drag?: SidebarRowDrag;
  /** A section label (a category) rather than a destination: quieter type. */
  variant?: "row" | "label";
  /** A destructive item (Delete category): the one red. */
  danger?: boolean;
}

export interface SidebarRowDrag {
  /** Spread on the row: pointer handlers and `data-drag-*` attributes. */
  rowProps: React.HTMLAttributes<HTMLDivElement> & Record<`data-${string}`, string | undefined>;
  /** This row is the one being dragged. */
  isDragging: boolean;
  dropEdge: DropEdge;
  /** Alt+↑ / Alt+↓ move the row one slot, for the keyboard. */
  onKeyDown?: (e: React.KeyboardEvent) => void;
}

export const SidebarRow: React.FC<SidebarRowProps> = ({
  indent,
  isActive,
  onClick,
  leading,
  chevron,
  label,
  badge,
  trailing,
  testId,
  dimmed,
  activateOn,
  onIntent,
  actions,
  actionsPinned,
  drag,
  variant = "row",
  danger = false,
}) => {
  const isLabel = variant === "label";
  return (
    <div
      data-active={isActive ? "true" : "false"}
      data-muted={dimmed ? "true" : undefined}
      data-tone={danger ? "danger" : isLabel ? "label" : undefined}
      onMouseEnter={onIntent}
      onFocus={onIntent}
      {...drag?.rowProps}
      className={`sidebar-row group/row relative flex w-full items-stretch border-s-2 transition-colors ${
        isActive
          ? "bg-hover border-accent text-accent"
          : `bg-transparent border-transparent hover:bg-hover ${
              danger ? "text-danger" : dimmed || isLabel ? "text-muted" : "text-fg"
            }`
      } ${isLabel ? "mt-1.5" : ""} ${drag?.isDragging ? "opacity-50" : ""} ${drag ? "touch-none select-none" : ""}`}
    >
      {drag?.dropEdge && (
        <span
          aria-hidden
          data-testid="sidebar-drop-line"
          className={`pointer-events-none absolute inset-x-2 h-0.5 rounded-full bg-accent ${
            drag.dropEdge === "before" ? "top-0" : "bottom-0"
          }`}
        />
      )}
      {chevron && (
        <PressButton
          tabIndex={-1}
          activateOn={activateOn ?? "click"}
          onClick={(e) => {
            e.stopPropagation();
            chevron.onToggle();
          }}
          aria-label={chevron.ariaLabel}
          className={`inline-flex items-center pe-0 cursor-pointer text-inherit ${indentPadClass(indent)}`}
        >
          {chevron.isCollapsed ? (
            <ChevronRight {...sidebarIconProps} className={`${sidebarIconProps.className} rtl-mirror`} />
          ) : (
            <ChevronDown {...sidebarIconProps} />
          )}
        </PressButton>
      )}
      <PressButton
        activateOn={activateOn ?? "click"}
        onClick={onClick}
        onKeyDown={drag?.onKeyDown}
        data-testid={testId ? `sidebar-row-${testId}` : undefined}
        className={`flex flex-1 min-w-0 items-center gap-1.5 py-0.5 pe-2.5 text-start cursor-pointer text-inherit ${
          isLabel ? "text-secondary font-medium" : "text-row"
        } ${chevron ? "ps-[0.4rem]" : indentPadClass(indent)}`}
      >
        {leading}
        {/* Isolated: user-generated names must not drag the badge in mixed-direction text. */}
        <bdi className="flex-1 truncate">{label}</bdi>
        {trailing}
        {badge != null && <SidebarUnreadBadge count={badge} muted={dimmed} />}
      </PressButton>
      {actions && (
        <div
          className={`flex shrink-0 items-center gap-0.5 pe-1.5 transition-opacity ${
            actionsPinned
              ? "opacity-100"
              : "opacity-0 group-hover/row:opacity-100 group-focus-within/row:opacity-100"
          }`}
        >
          {actions}
        </div>
      )}
    </div>
  );
};

export const SidebarUnreadBadge: React.FC<{ count: number; muted?: boolean }> = ({ count, muted }) => (
  <span
    className={`shrink-0 rounded-chip px-1.5 py-0.5 text-meta leading-none text-bg ${
      muted ? "bg-muted" : "bg-accent"
    }`}
  >
    {count > 99 ? "99+" : count}
  </span>
);
