import React from "react";

const Spinner = () => (
  <span
    className="inline-block w-3.5 h-3.5 rounded-full border-2 animate-spin flex-shrink-0"
    style={{ borderColor: "var(--c-accent)", borderTopColor: "transparent" }}
  />
);

interface ButtonProps {
  children: React.ReactNode;
  onClick?: (e: React.MouseEvent<HTMLButtonElement>) => void;
  disabled?: boolean;
  isLoading?: boolean;
  loadingText?: string;
  className?: string;
  variant?: "primary" | "secondary" | "danger" | "ghost";
  type?: "button" | "submit" | "reset";
  onKeyDown?: (e: React.KeyboardEvent<HTMLButtonElement>) => void;
  "aria-label"?: string;
  "data-testid"?: string;
}

export const Button: React.FC<ButtonProps> = ({
  children,
  onClick,
  disabled = false,
  isLoading = false,
  loadingText = "Loading...",
  className = "",
  variant = "primary",
  type = "button",
  onKeyDown,
  "aria-label": ariaLabel,
  "data-testid": testId,
}) => {
  const isPrimary = variant === "primary";
  const isDanger = variant === "danger";
  const isGhost = variant === "ghost";

  const variantStyles = (() => {
    if (isDanger) {
      return {
        border: "2px solid hsl(0 70% 50% / 40%)",
        background: "transparent",
        color: "hsl(0 70% 55%)",
      };
    }
    if (isPrimary) {
      return {
        border: "2px solid transparent",
        background: "var(--c-accent)",
        color: "var(--c-bg)",
      };
    }
    if (isGhost) {
      return {
        border: "none",
        background: "transparent",
        color: "var(--c-text-muted)",
      };
    }
    return {
      border: "2px solid var(--c-border-active)",
      background: "transparent",
      color: "var(--c-accent)",
    };
  })();

  return (
    <button
      type={type}
      onClick={isLoading ? undefined : (e) => { onClick?.(e); }}
      disabled={disabled || isLoading}
      onKeyDown={onKeyDown}
      aria-label={ariaLabel}
      data-testid={testId}
      className={`inline-flex items-center justify-center gap-2 px-4 py-2 font-mono text-xs font-medium transition-colors focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black ${className}`}
      style={{
        ...variantStyles,
        borderRadius: "4px",
        opacity: disabled || isLoading ? 0.5 : 1,
        cursor: disabled || isLoading ? "not-allowed" : "pointer",
        letterSpacing: "0.5px"
      }}
      onMouseEnter={(e) => {
        if (disabled || isLoading) { return; }
        const el = e.currentTarget as HTMLElement;
        if (isDanger) {
          el.style.background = "hsl(0 70% 50% / 10%)";
        } else if (isPrimary) {
          el.style.opacity = "0.85";
        } else if (isGhost) {
          el.style.color = "var(--c-text)";
        } else {
          el.style.background = "var(--c-hover)";
        }
      }}
      onMouseLeave={(e) => {
        if (disabled || isLoading) { return; }
        const el = e.currentTarget as HTMLElement;
        el.style.opacity = disabled || isLoading ? "0.5" : "1";
        el.style.background = variantStyles.background;
        if (isGhost) { el.style.color = "var(--c-text-muted)"; }
      }}
    >
      {isLoading && <Spinner />}
      {isLoading ? loadingText : children}
    </button>
  );
};
