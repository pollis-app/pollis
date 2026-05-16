import React from "react";

interface SwitchProps {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  className?: string;
  id?: string;
  description?: string;
  "data-testid"?: string;
}

export const Switch: React.FC<SwitchProps> = ({
  label,
  checked,
  onChange,
  disabled = false,
  className = "",
  id,
  description,
  "data-testid": testId,
}) => {
  const inputId = id || `switch-${label.toLowerCase().replace(/\s+/g, "-")}`;
  const descriptionId = description ? `${inputId}-description` : undefined;

  return (
    <div className={`flex flex-col gap-1 ${className}`}>
      <div className="flex items-center gap-3">
        <button
          id={inputId}
          data-testid={testId}
          type="button"
          role="switch"
          aria-checked={checked}
          aria-describedby={descriptionId}
          onClick={() => { if (!disabled) { onChange(!checked); } }}
          disabled={disabled}
          className="relative inline-flex h-5 w-9 flex-shrink-0 items-center rounded-full transition-colors duration-200 focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black disabled:opacity-50 disabled:cursor-not-allowed"
          style={{
            background: checked ? "var(--c-accent)" : "var(--c-border-active)",
            cursor: disabled ? "not-allowed" : "pointer",
          }}
        >
          <span
            className="inline-block h-4 w-4 transform rounded-full transition-transform duration-200"
            style={{
              background: "var(--c-bg)",
              // Track is w-9 (2.25rem), thumb is w-4 (1rem). Express travel
              // in rem (not px) so it scales with the same root font size the
              // track does — keeps the dot centered with an even 0.125rem
              // inset at any font-size preference.
              transform: checked
                ? "translateX(1.125rem)"
                : "translateX(0.125rem)",
            }}
          />
        </button>

        <label
          htmlFor={inputId}
          className="text-sm font-mono"
          style={{
            color: "var(--c-text-dim)",
            cursor: disabled ? "not-allowed" : "pointer",
          }}
        >
          {label}
        </label>
      </div>

      {description && (
        <p
          id={descriptionId}
          className="text-xs font-mono ml-12"
          style={{ color: "var(--c-text-muted)" }}
        >
          {description}
        </p>
      )}
    </div>
  );
};
