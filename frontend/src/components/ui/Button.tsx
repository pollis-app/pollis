import React from "react";
import { useTranslation } from "react-i18next";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

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
  /** Hover / keyboard-focus intent, e.g. to pre-warm what the click will need (#1639). */
  onPointerEnter?: (e: React.PointerEvent<HTMLButtonElement>) => void;
  onFocus?: (e: React.FocusEvent<HTMLButtonElement>) => void;
  autoFocus?: boolean;
  "aria-label"?: string;
  /** Toggle-button selected state; otherwise only colour distinguishes it. */
  "aria-pressed"?: boolean;
  "data-testid"?: string;
  /** Last copy outcome (#889, #898): `idle`, `copied` or `failed`. */
  "data-copy-state"?: string;
  /** `"press"` acts on mouse pointer-down (#1529); only for actions free to undo. Default `"click"`. */
  activateOn?: ActivateOn;
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
  onPointerEnter,
  onFocus,
  autoFocus,
  "aria-label": ariaLabel,
  "aria-pressed": ariaPressed,
  "data-testid": testId,
  "data-copy-state": copyState,
  activateOn,
}) => {
  const { t } = useTranslation("common");
  const resolvedLoadingText = loadingText ?? t("states.buttonLoading");
  const isPrimary = variant === "primary";
  const isDanger = variant === "danger";
  const isGhost = variant === "ghost";
  const press = usePressActivation<HTMLButtonElement>(
    isLoading ? undefined : (e) => { onClick?.(e); },
    activateOn,
    disabled || isLoading,
  );

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
      {...press}
      disabled={disabled || isLoading}
      onKeyDown={onKeyDown}
      onPointerEnter={onPointerEnter}
      onFocus={onFocus}
      autoFocus={autoFocus}
      aria-label={ariaLabel}
      aria-pressed={ariaPressed}
      data-testid={testId}
      data-copy-state={copyState}
      className={`inline-flex items-center justify-center gap-2 font-mono font-medium rounded-control tracking-[0.5px] cursor-pointer transition-colors disabled:opacity-50 disabled:cursor-not-allowed focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black ${variantClass} ${size === "xs" ? "px-1.5 py-0.5 text-[10px]" : size === "sm" ? "px-2.5 py-1 text-[11px]" : "px-4 py-2 text-xs"} ${className}`}
    >
      {isLoading && <Spinner />}
      {isLoading ? resolvedLoadingText : children}
    </button>
  );
};
