import React, { useState } from "react";
import { Eye, EyeOff } from "lucide-react";

interface PasswordInputProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  id: string;
  "data-testid"?: string;
  autoComplete?: string;
  strength?: {
    label: string;
    color: string;
  };
}

export const PasswordInput: React.FC<PasswordInputProps> = ({
  label,
  value,
  onChange,
  placeholder,
  id,
  "data-testid": testId,
  autoComplete,
  strength,
}) => {
  const [visible, setVisible] = useState(false);

  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="section-label px-0">{label}</label>
      <div className="flex gap-1">
        <input
          id={id}
          data-testid={testId}
          type={visible ? "text" : "password"}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          autoComplete={autoComplete}
          className="pollis-input flex-1"
        />
        <button
          type="button"
          onClick={() => setVisible((v) => !v)}
          aria-label="Toggle password visibility"
          className="icon-btn"
        >
          {visible ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
        </button>
      </div>
      {value && strength && (
        <span className="text-2xs font-mono" style={{ color: strength.color }}>
          {strength.label}
        </span>
      )}
    </div>
  );
};
