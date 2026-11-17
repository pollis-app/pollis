import React from "react";
import { FOCUS_RING } from "./focusRing";

interface PillButtonProps {
  accent: string;
  onClick?: () => void;
  title?: string;
  square?: boolean;
  children: React.ReactNode;
  "data-testid"?: string;
  "aria-label"?: string;
  /** Toggle state for assistive tech; omitted for plain actions. */
  "aria-pressed"?: boolean;
  /** Any `data-*` attribute, forwarded verbatim (e.g. `data-mic-state`, #849). */
  [dataAttribute: `data-${string}`]: unknown;
}

/** Filled accent pill that inverts to outlined on hover; `square` for icon-only. */
export const PillButton: React.FC<PillButtonProps> = ({
  accent,
  onClick,
  title,
  square = false,
  children,
  "data-testid": testId,
  "aria-label": ariaLabel,
  "aria-pressed": ariaPressed,
  ...rest
}) => {
  // Only `data-*`: className and style stay owned here.
  const dataAttributes = Object.fromEntries(
    Object.entries(rest).filter(([key]) => key.startsWith("data-")),
  );

  return (
    <button
      data-testid={testId}
      {...dataAttributes}
      aria-label={ariaLabel}
      aria-pressed={ariaPressed}
      title={title}
      onClick={onClick}
      className={`flex items-center justify-center font-mono leading-[1.4] transition-colors cursor-pointer rounded-chip border border-solid border-[var(--pill-accent)] bg-[var(--pill-accent)] text-bg hover:bg-bg hover:text-[var(--pill-accent)] ${
        square ? "p-1" : "px-2 py-0.5 gap-1.5"
      } ${FOCUS_RING}`}
      // The accent is a runtime colour (the voice state's), so it rides a CSS variable.
      style={{ ["--pill-accent" as string]: accent } as React.CSSProperties}
    >
      {children}
    </button>
  );
};
