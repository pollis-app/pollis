import React, { useState } from "react";
import { ChevronRight } from "lucide-react";

interface TextAreaProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  description?: string;
  error?: string;
  disabled?: boolean;
  rows?: number;
  className?: string;
  id?: string;
}

export const TextArea: React.FC<TextAreaProps> = ({
  label,
  value,
  onChange,
  placeholder,
  description,
  error,
  disabled = false,
  rows = 3,
  className = "",
  id,
}) => {
  const [isFocused, setIsFocused] = useState(false);
  const inputId = id || `textarea-${Math.random().toString(36).substr(2, 9)}`;

  return (
    <div className={`relative w-full ${className}`}>
      <label
        htmlFor={inputId}
        className="block text-xs font-mono font-medium mb-1.5"
        style={{ color: "var(--c-text-dim)" }}
      >
        {label}
      </label>
      <div className="relative">
        {isFocused && !disabled && (
          <ChevronRight
            className="absolute left-2 top-3 w-3 h-3 pointer-events-none"
            style={{ color: "var(--c-accent)" }}
          />
        )}
        <textarea
          id={inputId}
          value={value}
          rows={rows}
          onChange={(e) => onChange(e.target.value)}
          onFocus={() => setIsFocused(true)}
          onBlur={() => setIsFocused(false)}
          placeholder={placeholder}
          disabled={disabled}
          aria-invalid={!!error}
          className="w-full py-2 font-mono text-sm resize-none focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black transition-all"
          style={{
            paddingLeft: isFocused && !disabled ? "1.5rem" : "0.75rem",
            paddingRight: "0.75rem",
            background: "var(--c-surface)",
            color: "var(--c-text)",
            border: `2px solid ${error ? "#ff6b6b" : isFocused ? "var(--c-border-active)" : "var(--c-border)"}`,
            outline: "none",
            borderRadius: "0.5rem",
            opacity: disabled ? 0.5 : 1,
            cursor: disabled ? "not-allowed" : undefined,
          }}
        />
      </div>
      {description && !error && (
        <p className="mt-1 text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          {description}
        </p>
      )}
      {error && (
        <p className="mt-1 text-xs font-mono" style={{ color: "#ff6b6b" }} role="alert">
          {error}
        </p>
      )}
    </div>
  );
};
