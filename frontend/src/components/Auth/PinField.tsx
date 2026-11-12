import React from "react";
import { InputOtp } from "../ui/InputOtp";

interface PinFieldProps {
  value: string;
  /** Called with digits only, at most four. */
  onChange: (pin: string) => void;
  disabled: boolean;
  /** Test id of the hidden input that mirrors the PIN for e2e. */
  "data-testid": string;
}

/** Masked, autofocused four-digit PIN boxes, plus a hidden input mirroring the value for e2e. */
export const PinField: React.FC<PinFieldProps> = ({
  value,
  onChange,
  disabled,
  "data-testid": testId,
}) => (
  <div>
    <InputOtp
      length={4}
      value={value}
      onChange={(v) => onChange(v.replace(/\D/g, "").slice(0, 4))}
      disabled={disabled}
      autoFocus
      mask
    />
    <input
      data-testid={testId}
      type="hidden"
      value={value}
      readOnly
    />
  </div>
);
