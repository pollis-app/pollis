import React from "react";
import { useTranslation } from "react-i18next";

const Spinner = () => (
  <span
    className="inline-block w-3.5 h-3.5 rounded-full border-2 border-accent animate-spin flex-shrink-0"
    style={{ borderTopColor: "transparent" }}
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
  /**
   * Selected state for a button acting as a toggle — e.g. one option in a
   * group of mutually exclusive choices. Without it a "selected" option is
   * announced identically to an unselected one, since the distinction is
   * carried only by the `primary` variant's colour.
   */
  "aria-pressed"?: boolean;
  "data-testid"?: string;
}

export const Button: React.FC<ButtonProps> = ({
  children,
  onClick,
  disabled = false,
  isLoading = false,
  loadingText,
  className = "",
  variant = "primary",
  size = "md",
  type = "button",
  onKeyDown,
  autoFocus,
  "aria-label": ariaLabel,
  "aria-pressed": ariaPressed,
  "data-testid": testId,
}) => {
  const { t } = useTranslation("common");
  // Resolved here rather than as a default parameter value so the fallback
  // follows a language change instead of snapshotting the boot language.
  const resolvedLoadingText = loadingText ?? t("states.buttonLoading");
  const isPrimary = variant === "primary";
  const isDanger = variant === "danger";
  const isGhost = variant === "ghost";

  const variantClass = isDanger
    ? "border-2 border-[hsl(0_70%_50%/0.4)] bg-transparent text-[hsl(0_70%_55%)] enabled:hover:bg-[hsl(0_70%_50%/0.1)]"
    : isPrimary
      ? "border-2 border-transparent bg-accent text-bg enabled:hover:opacity-[0.85]"
      : isGhost
        ? "border-none bg-transparent text-muted enabled:hover:text-fg"
        : "border-2 border-line-strong bg-transparent text-accent enabled:hover:bg-hover";

  return (
    <button
      type={type}
      onClick={isLoading ? undefined : (e) => { onClick?.(e); }}
      disabled={disabled || isLoading}
      onKeyDown={onKeyDown}
      autoFocus={autoFocus}
      aria-label={ariaLabel}
      aria-pressed={ariaPressed}
      data-testid={testId}
      className={`inline-flex items-center justify-center gap-2 font-mono font-medium rounded-control tracking-[0.5px] cursor-pointer transition-colors disabled:opacity-50 disabled:cursor-not-allowed focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black ${variantClass} ${size === "xs" ? "px-1.5 py-0.5 text-[10px]" : size === "sm" ? "px-2.5 py-1 text-[11px]" : "px-4 py-2 text-xs"} ${className}`}
    >
      {isLoading && <Spinner />}
      {isLoading ? resolvedLoadingText : children}
    </button>
  );
};
