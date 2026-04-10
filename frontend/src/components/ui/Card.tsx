import React from "react";

interface CardProps {
  children: React.ReactNode;
  className?: string;
  style?: React.CSSProperties;
  padding?: "sm" | "md" | "lg" | "none";
  "data-testid"?: string;
}

const paddingMap = {
  none: "0",
  sm: "1rem",
  md: "1.5rem",
  lg: "2rem",
};

export const Card: React.FC<CardProps> = ({
  children,
  className = "",
  style,
  padding = "md",
  "data-testid": testId,
}) => (
  <div
    data-testid={testId}
    className={className}
    style={{
      background: "var(--c-surface)",
      border: "2px solid var(--c-border)",
      borderRadius: "6px",
      padding: paddingMap[padding],
      ...style,
    }}
  >
    {children}
  </div>
);
