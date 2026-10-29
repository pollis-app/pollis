import React from "react";

interface PillButtonProps {
  accent: string;
  onClick?: () => void;
  title?: string;
  /** Icon-only square variant. */
  square?: boolean;
  children: React.ReactNode;
  "data-testid"?: string;
  "aria-label"?: string;
  /** Any `data-*` attribute, forwarded verbatim (e.g. `data-mic-state`, #849). */
  [dataAttribute: `data-${string}`]: unknown;
}

/**
 * Filled accent pill that inverts to outlined on hover, for tight inline
 * contexts where colour carries meaning. `square` for icon-only.
 */
export const PillButton: React.FC<PillButtonProps> = ({
  accent,
  onClick,
  title,
  square = false,
  children,
  "data-testid": testId,
  "aria-label": ariaLabel,
  ...rest
}) => {
  // Only `data-*` passes through; className and style stay owned here.
  const dataAttributes = Object.fromEntries(
    Object.entries(rest).filter(([key]) => key.startsWith("data-")),
  );

  return (
    <button
      data-testid={testId}
      {...dataAttributes}
      aria-label={ariaLabel}
      title={title}
      onClick={onClick}
      className="flex items-center justify-center font-mono transition-colors cursor-pointer rounded-[3px] border border-solid border-[var(--pill-accent)] bg-[var(--pill-accent)] text-bg hover:bg-bg hover:text-[var(--pill-accent)]"
      style={{
        ["--pill-accent" as string]: accent,
        padding: square ? "3px" : "1px 8px",
        gap: square ? 0 : "0.375rem",
        lineHeight: 1.4,
      } as React.CSSProperties}
    >
      {children}
    </button>
  );
};
