import React from "react";

const Spinner = () => (
  <span
    className="inline-block w-3.5 h-3.5 rounded-full border-2 animate-spin flex-shrink-0"
    style={{ borderColor: "var(--c-accent)", borderTopColor: "transparent" }}
  />
);

interface ButtonProps {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  isLoading?: boolean;
  loadingText?: string;
  className?: string;
  variant?: "primary" | "secondary";
  type?: "button" | "submit" | "reset";
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
  "aria-label": ariaLabel,
  "data-testid": testId,
}) => {
  const isPrimary = variant === "primary";

  return (
    <button
      type={type}
      onClick={isLoading ? undefined : onClick}
      disabled={disabled || isLoading}
      aria-label={ariaLabel}
      data-testid={testId}
      className={`inline-flex items-center justify-center gap-2 px-4 py-2 font-sans text-sm font-medium transition-colors ${className}`}
      style={{
        border: `1px solid var(--c-border-active)`,
        borderRadius: "4px",
        background: isPrimary ? "var(--c-accent)" : "transparent",
        color: isPrimary ? "var(--c-bg)" : "var(--c-accent)",
        opacity: disabled || isLoading ? 0.5 : 1,
        cursor: disabled || isLoading ? "not-allowed" : "pointer",
        letterSpacing: "0.5px"
      }}
      onMouseEnter={(e) => {
        if (disabled || isLoading) { return; }
        const el = e.currentTarget as HTMLElement;
        if (isPrimary) {
          el.style.opacity = "0.85";
        } else {
          el.style.background = "var(--c-hover)";
        }
      }}
      onMouseLeave={(e) => {
        if (disabled || isLoading) { return; }
        const el = e.currentTarget as HTMLElement;
        el.style.opacity = disabled || isLoading ? "0.5" : "1";
        el.style.background = isPrimary ? "var(--c-accent)" : "transparent";
      }}
    >
      {isLoading && <Spinner />}
      {isLoading ? loadingText : children}
    </button>
  );
};
