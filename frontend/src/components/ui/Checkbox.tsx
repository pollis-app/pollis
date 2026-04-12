import React from "react";

interface CheckboxProps {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  className?: string;
  "data-testid"?: string;
}

export const Checkbox: React.FC<CheckboxProps> = ({
  label,
  checked,
  onChange,
  disabled = false,
  className = "",
  "data-testid": testId,
}) => {
  return (
    <label
      className={`flex items-start gap-3 font-mono text-xs ${className}`}
      style={{
        color: "var(--c-text-dim)",
        cursor: disabled ? "not-allowed" : "pointer",
        lineHeight: 1.6,
      }}
    >
      <span
        data-testid={testId}
        role="checkbox"
        aria-checked={checked}
        tabIndex={0}
        onClick={() => {
          if (!disabled) {
            onChange(!checked);
          }
        }}
        onKeyDown={(e) => {
          if (!disabled && (e.key === " " || e.key === "Enter")) {
            e.preventDefault();
            onChange(!checked);
          }
        }}
        className="flex-shrink-0 flex items-center justify-center mt-px"
        style={{
          width: 16,
          height: 16,
          borderRadius: 3,
          border: `2px solid ${checked ? "var(--c-accent)" : "var(--c-border-active)"}`,
          background: checked ? "var(--c-accent)" : "transparent",
          transition: "background 150ms, border-color 150ms",
          opacity: disabled ? 0.5 : 1,
          cursor: disabled ? "not-allowed" : "pointer",
        }}
      >
        {checked && (
          <svg
            width="10"
            height="10"
            viewBox="0 0 10 10"
            fill="none"
            xmlns="http://www.w3.org/2000/svg"
          >
            <path
              d="M2 5L4 7L8 3"
              stroke="var(--c-bg)"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        )}
      </span>
      <span>{label}</span>
    </label>
  );
};
