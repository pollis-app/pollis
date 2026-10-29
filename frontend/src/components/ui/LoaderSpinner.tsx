import React from "react";
import { useTranslation } from "react-i18next";

/**
 * @interface LoadingSpinnerProps
 */
interface LoadingSpinnerProps {
  size?: "sm" | "base" | "lg";
  className?: string;
}

/**
 * An animated loading spinner component using classic CLI-style characters.
 * @component
 * @param {LoadingSpinnerProps} props - The props for the LoadingSpinner component
 * @param {'sm' | 'base' | 'lg'} [props.size='base'] - The size variant affecting font size
 * @param {string} [props.className] - Additional CSS classes to apply to the spinner
 *
 * @example
 * ```tsx
 * // Basic usage with default size
 * <LoadingSpinner />
 *
 * // Small spinner for compact spaces
 * <LoadingSpinner size="sm" />
 *
 * // Large spinner for prominent loading states
 * <LoadingSpinner size="lg" />
 *
 * // With custom styling
 * <LoadingSpinner
 *   size="lg"
 *   className="text-center my-4"
 * />
 *
 * // In a button during loading
 * <Button disabled={isLoading}>
 *   {isLoading ? <LoadingSpinner size="sm" /> : 'Submit'}
 * </Button>
 *
 * // Different size variants
 * <LoadingSpinner size="sm" />
 * <LoadingSpinner size="base" />
 * <LoadingSpinner size="lg" />
 * ```
 *
 * @returns {JSX.Element} An animated loading spinner with the specified size and styling
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