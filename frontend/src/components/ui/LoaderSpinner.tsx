import React from "react";

interface LoadingSpinnerProps {
  size?: "sm" | "base" | "lg";
  className?: string;
}

const sizeClasses = {
  sm: "text-xs",
  base: "text-2xl",
  lg: "text-5xl",
};

// Animation is pure CSS — see the `loader-spinner` rule in index.css.
// No JS timer, no re-renders.
export const LoadingSpinner: React.FC<LoadingSpinnerProps> = ({
  size = "base",
  className = "",
}) => {
  return (
    <span
      className={`loader-spinner inline-block font-mono ${sizeClasses[size]} ${className}`}
      style={{ color: "var(--c-accent)" }}
      aria-label="Loading"
    />
  );
};
