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
  size?: "xs" | "sm" | "md";
  type?: "button" | "submit" | "reset";
  onKeyDown?: (e: React.KeyboardEvent<HTMLButtonElement>) => void;
  autoFocus?: boolean;
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
  size = "md",
  type = "button",
  onKeyDown,
  autoFocus,
  "aria-label": ariaLabel,
  "data-testid": testId,
}) => {
  const isPrimary = variant === "primary";
  const isDanger = variant === "danger";
  const isGhost = variant === "ghost";

  const variantClass = isDanger
    ? "border-2 border-[hsl(0_70%_50%/0.4)] bg-transparent text-[hsl(0_70%_55%)] enabled:hover:bg-[hsl(0_70%_50%/0.1)]"
    : isPrimary
      ? "border-2 border-transparent bg-[var(--c-accent)] text-[var(--c-bg)] enabled:hover:opacity-[0.85]"
      : isGhost
        ? "border-none bg-transparent text-[var(--c-text-muted)] enabled:hover:text-[var(--c-text)]"
        : "border-2 border-[var(--c-border-active)] bg-transparent text-[var(--c-accent)] enabled:hover:bg-[var(--c-hover)]";

  return (
    <button
      type={type}
      onClick={isLoading ? undefined : (e) => { onClick?.(e); }}
      disabled={disabled || isLoading}
      onKeyDown={onKeyDown}
      autoFocus={autoFocus}
      aria-label={ariaLabel}
      data-testid={testId}
      className={`inline-flex items-center justify-center gap-2 font-mono font-medium rounded-[4px] tracking-[0.5px] cursor-pointer transition-colors disabled:opacity-50 disabled:cursor-not-allowed focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black ${variantClass} ${size === "xs" ? "px-1.5 py-0.5 text-[10px]" : size === "sm" ? "px-2.5 py-1 text-[11px]" : "px-4 py-2 text-xs"} ${className}`}
    >
      {isLoading && <Spinner />}
      {isLoading ? loadingText : children}
    </button>
  );
};
