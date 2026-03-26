import React, { useState } from "react";
import { ChevronRight } from "lucide-react";

interface TextInputProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  description?: string;
  error?: string;
  disabled?: boolean;
  type?: "text" | "password" | "email" | "number";
  className?: string;
  id?: string;
  required?: boolean;
  "data-testid"?: string;
  autoFocus?: boolean;
  autoComplete?: string;
}

export const TextInput: React.FC<TextInputProps> = ({
  label,
  value,
  onChange,
  placeholder,
  description,
  error,
  disabled = false,
  type = "text",
  className = "",
  id,
  required = false,
  "data-testid": testId,
  autoFocus,
  autoComplete,
}) => {
  const [isFocused, setIsFocused] = useState(false);
  const inputId = id || `input-${Math.random().toString(36).substr(2, 9)}`;

  return (
    <div className={`relative w-full ${className}`}>
      <label
        htmlFor={inputId}
        className="block text-xs font-sans font-medium mb-1.5"
        style={{ color: "var(--c-text)", letterSpacing: "0.5px" }}
      >
        {label}
        {required && <span className="ml-1" style={{ color: "#ff6b6b" }}>*</span>}
      </label>

      <div className="relative">
        {isFocused && !disabled && (
          <ChevronRight
            className="absolute left-2 top-1/2 -translate-y-1/2 w-3 h-3 pointer-events-none"
            style={{ color: "var(--c-accent)" }}
          />
        )}
        <input
          id={inputId}
          data-testid={testId}
          type={type}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onFocus={() => setIsFocused(true)}
          onBlur={() => setIsFocused(false)}
          placeholder={placeholder}
          disabled={disabled}
          autoFocus={autoFocus}
          autoComplete={autoComplete ?? "off"}
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          required={required}
          aria-invalid={!!error}
          className="w-full py-2 placeholder-leading-1 font-mono text-sm focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black transition-all"
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
        <p className="mt-1 text-xs font-sans" style={{ color: "var(--c-text-muted)" }}>
          {description}
        </p>
      )}
      {error && (
        <p className="mt-1 text-xs font-sans" style={{ color: "#ff6b6b" }} role="alert">
          {error}
        </p>
      )}
    </div>
  );
};
