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
}) => {
  return (
    <button
      data-testid={testId}
      aria-label={ariaLabel}
      title={title}
      onClick={onClick}
      className="flex items-center justify-center font-mono transition-colors"
      style={{
        background: accent,
        color: "var(--c-bg)",
        border: `1px solid ${accent}`,
        padding: square ? "3px" : "1px 8px",
        gap: square ? 0 : "0.375rem",
        borderRadius: 3,
        cursor: "pointer",
        lineHeight: 1.4,
      }}
      onMouseEnter={(e) => {
        const el = e.currentTarget as HTMLButtonElement;
        el.style.background = "var(--c-bg)";
        el.style.color = accent;
      }}
      onMouseLeave={(e) => {
        const el = e.currentTarget as HTMLButtonElement;
        el.style.background = accent;
        el.style.color = "var(--c-bg)";
      }}
    >
      {children}
    </button>
  );
};
