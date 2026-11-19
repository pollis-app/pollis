import React, { useEffect, useRef } from "react";
import { FOCUS_RING } from "./focusRing";

interface InlineInputProps {
  value: string;
  onChange: (value: string) => void;
  /** Enter. */
  onSubmit: () => void;
  /** Escape. */
  onCancel: () => void;
  ariaLabel: string;
  placeholder?: string;
  disabled?: boolean;
  "data-testid"?: string;
  maxLength?: number;
}

/**
 * A one-line field edited in place, inside a list row (a sidebar category's name, #2031): no
 * label and no border, the raised fill and the one focus ring. Takes focus and selects its text
 * on mount; Enter submits, Escape cancels (and is claimed, so it does not also navigate back).
 * A form's field is `TextInput`.
 */
export const InlineInput: React.FC<InlineInputProps> = ({
  value,
  onChange,
  onSubmit,
  onCancel,
  ariaLabel,
  placeholder,
  disabled = false,
  "data-testid": testId,
  maxLength,
}) => {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  return (
    <input
      ref={ref}
      data-testid={testId}
      type="text"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          onSubmit();
        } else if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          e.nativeEvent.stopImmediatePropagation();
          onCancel();
        }
      }}
      aria-label={ariaLabel}
      placeholder={placeholder}
      disabled={disabled}
      maxLength={maxLength}
      autoComplete="off"
      autoCorrect="off"
      spellCheck={false}
      className={`w-full min-w-0 rounded-control bg-surface-raised px-2 py-0.5 text-secondary text-fg placeholder:text-muted outline-none ${FOCUS_RING} ${
        disabled ? "opacity-50" : ""
      }`}
    />
  );
};
