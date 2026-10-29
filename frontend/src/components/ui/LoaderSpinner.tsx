import React from "react";
import { useTranslation } from "react-i18next";

interface LoadingSpinnerProps {
  size?: "sm" | "base" | "lg";
  className?: string;
}

/**
 * A braille CLI-style loading spinner, animated purely in CSS (no JS timer).
 *
 * ```tsx
 * <LoadingSpinner size="sm" />
 * ```
 */
export const LoadingSpinner: React.FC<LoadingSpinnerProps> = ({
  size = "base",
  className = "",
}) => {
  const { t } = useTranslation("common");
  const sizeClasses = {
    sm: "text-xs",
    base: "text-2xl",
    lg: "text-5xl",
  };

  return (
    <span
      className={`loader-spinner inline-block font-mono text-accent ${sizeClasses[size]} ${className}`}
      aria-label={t("a11y.loading")}
    />
  );
};