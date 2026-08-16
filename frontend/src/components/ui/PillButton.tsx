import React from "react";

interface PillButtonProps {
  accent: string;
  onClick?: () => void;
  title?: string;
  /** Icon-only square variant — equal padding on all sides, no inner gap. */
  square?: boolean;
  children: React.ReactNode;
  "data-testid"?: string;
  "aria-label"?: string;
  /**
   * Any other `data-*` attribute, forwarded verbatim to the button.
   *
   * State that callers publish for the tray and for tests to read — the voice
   * bar's `data-mic-state` / `data-deafened` (#849) — arrives this way. Before
   * this existed the props were a closed list, so those attributes were
   * accepted by TypeScript and then silently dropped on the floor.
   */
  [dataAttribute: `data-${string}`]: unknown;
}

/**
 * Filled accent-colored pill that inverts to outlined on hover. Used in
 * tight inline contexts (e.g. the bottom voice bar) where the affordance
 * needs to read as clickable at a glance and color itself carries meaning
 * (orange = active, red = destructive, etc.). Pass `square` for an
 * icon-only variant.
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
  // Only `data-*` goes through: className and style stay owned by this
  // component, so a caller cannot quietly unpick the pill's appearance.
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
