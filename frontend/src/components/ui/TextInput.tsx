import React, { useState } from "react";
import { ChevronRight } from "lucide-react";
import { fieldControlClass, fieldControlStyle } from "./fieldStyles";

interface TextInputProps {
  /** Omit only when context names the field, and pass `ariaLabel` then. */
  label?: string;
  ariaLabel?: string;
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
  ariaLabel,
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
      {label && (
        <label
          htmlFor={inputId}
          className="block text-xs font-sans font-medium mb-1.5 text-fg"
          style={{ letterSpacing: "0.5px" }}
        >
          {label}
          {required && <span className="ms-1 text-danger">*</span>}
        </label>
      )}

      <div className="relative">
        {isFocused && !disabled && (
          <ChevronRight
            className="absolute start-2 top-1/2 -translate-y-1/2 w-3 h-3 pointer-events-none rtl-mirror text-accent"
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
          aria-label={label ? undefined : ariaLabel}
          aria-invalid={!!error}
          className={fieldControlClass("placeholder-leading-1", error, isFocused)}
          style={fieldControlStyle(isFocused, disabled)}
        />
      </div>

      {description && !error && (
        <p className="mt-1 text-xs font-sans text-muted">
          {description}
        </p>
      )}
      {error && (
        <p className="mt-1 text-xs font-sans text-danger" role="alert">
          {error}
        </p>
      )}
    </div>
  );
};
