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
  /**
   * Key handler on the underlying `<textarea>`. Added for the thread composer
   * (#825), which needs Enter-to-send with Shift+Enter for a newline — the
   * same contract as the main chat input.
   */
  onKeyDown?: React.KeyboardEventHandler<HTMLTextAreaElement>;
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
  onKeyDown,
}) => {
  const [isFocused, setIsFocused] = useState(false);
  const inputId = id || `textarea-${Math.random().toString(36).substr(2, 9)}`;

  return (
    <div className={`relative w-full ${className}`}>
      <label
        htmlFor={inputId}
        className="block text-xs font-mono font-medium mb-1.5 text-dim"
      >
        {label}
      </label>
      <div className="relative">
        {isFocused && !disabled && (
          <ChevronRight
            className="absolute start-2 top-3 w-3 h-3 pointer-events-none rtl-mirror text-accent"
          />
        )}
        <textarea
          id={inputId}
          value={value}
          rows={rows}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={onKeyDown}
          onFocus={() => setIsFocused(true)}
          onBlur={() => setIsFocused(false)}
          placeholder={placeholder}
          disabled={disabled}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          aria-invalid={!!error}
          className={`w-full py-2 font-mono text-sm resize-none focus:outline-none focus:ring-4 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black transition-all bg-surface text-fg border-2 ${
            error ? "border-danger" : isFocused ? "border-line-strong" : "border-line"
          }`}
          style={{
            paddingInlineStart: isFocused && !disabled ? "1.5rem" : "0.75rem",
            paddingInlineEnd: "0.75rem",
            outline: "none",
            borderRadius: "0.5rem",
            opacity: disabled ? 0.5 : 1,
            cursor: disabled ? "not-allowed" : undefined,
          }}
        />
      </div>
      {description && !error && (
        <p className="mt-1 text-xs font-mono text-muted">
          {description}
        </p>
      )}
      {error && (
        <p className="mt-1 text-xs font-mono text-danger" role="alert">
          {error}
        </p>
      )}
    </div>
  );
};
