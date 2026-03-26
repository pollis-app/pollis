import React, { useRef, useState } from "react";

interface InputOtpProps {
  length?: number;
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  autoFocus?: boolean;
}

export const InputOtp: React.FC<InputOtpProps> = ({
  length = 6,
  value,
  onChange,
  disabled = false,
  autoFocus = false,
}) => {
  const [focusedIndex, setFocusedIndex] = useState<number | null>(null);
  const inputsRef = useRef<(HTMLInputElement | null)[]>([]);

  const digits = value.split("").concat(Array(length).fill("")).slice(0, length);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>, index: number) => {
    if (e.key === "Backspace") {
      if (digits[index]) {
        const next = digits.map((d, i) => (i === index ? "" : d)).join("").replace(/ /g, "");
        onChange(next);
      } else if (index > 0) {
        inputsRef.current[index - 1]?.focus();
        const next = digits.map((d, i) => (i === index - 1 ? "" : d)).join("").replace(/ /g, "");
        onChange(next);
      }
      e.preventDefault();
    } else if (e.key === "ArrowLeft" && index > 0) {
      inputsRef.current[index - 1]?.focus();
    } else if (e.key === "ArrowRight" && index < length - 1) {
      inputsRef.current[index + 1]?.focus();
    }
  };

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>, index: number) => {
    const raw = e.target.value.replace(/\D/g, "");
    if (!raw) {
      return;
    }
    // Support pasting multiple digits
    const chars = raw.slice(0, length - index).split("");
    const next = [...digits];
    chars.forEach((ch, i) => {
      if (index + i < length) {
        next[index + i] = ch;
      }
    });
    onChange(next.join(""));
    const nextFocus = Math.min(index + chars.length, length - 1);
    inputsRef.current[nextFocus]?.focus();
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLInputElement>, index: number) => {
    e.preventDefault();
    const pasted = e.clipboardData.getData("text").replace(/\D/g, "").slice(0, length - index);
    if (!pasted) { return; }
    const next = [...digits];
    pasted.split("").forEach((ch, i) => {
      if (index + i < length) { next[index + i] = ch; }
    });
    onChange(next.join(""));
    const nextFocus = Math.min(index + pasted.length, length - 1);
    inputsRef.current[nextFocus]?.focus();
  };

  const handleFocus = (e: React.FocusEvent<HTMLInputElement>, index: number) => {
    setFocusedIndex(index);
    e.target.select();
  };

  return (
    <div className="flex items-center gap-2">
      {digits.map((digit, index) => {
        const isFocused = focusedIndex === index;
        return (
          <input
            key={index}
            ref={(el) => { inputsRef.current[index] = el; }}
            type="text"
            inputMode="numeric"
            maxLength={1}
            value={digit}
            onChange={(e) => handleChange(e, index)}
            onKeyDown={(e) => handleKeyDown(e, index)}
            onPaste={(e) => handlePaste(e, index)}
            onFocus={(e) => handleFocus(e, index)}
            onBlur={() => setFocusedIndex(null)}
            disabled={disabled}
            autoFocus={autoFocus && index === 0}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            aria-label={`OTP digit ${index + 1}`}
            className="w-10 h-12 text-center font-mono text-lg font-medium transition-all"
            style={{
              background: isFocused ? "var(--c-accent)" : "var(--c-surface)",
              color: isFocused ? "var(--c-bg)" : "var(--c-text)",
              border: `1px solid ${isFocused ? "var(--c-accent)" : "var(--c-border)"}`,
              outline: "none",
              borderRadius: "4px",
              opacity: disabled ? 0.5 : 1,
              cursor: disabled ? "not-allowed" : "text",
            }}
          />
        );
      })}
    </div>
  );
};
