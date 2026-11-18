import React from "react";
import { TextInput } from "./TextInput";
import { SAS_LENGTH, normalizeSasInput } from "../../utils/enrollmentSas";

interface SasCodeInputProps {
  /** Names where the code comes from ("Code shown on the new device"). */
  label: string;
  description?: string;
  value: string;
  /** Receives the normalized code: upper-case, alphabet only, at most `SAS_LENGTH`. */
  onChange: (code: string) => void;
  disabled?: boolean;
  autoFocus?: boolean;
  "data-testid"?: string;
}

/**
 * The field a person types a short code into, read off another screen: enrollment approval
 * (#1096) and bot pairing (#1182). Always typed, never pre-filled or pasted in by the app, so a
 * server that swaps the key behind the code fails the comparison. `utils/enrollmentSas.ts` holds
 * the alphabet and the input rule.
 */
export const SasCodeInput: React.FC<SasCodeInputProps> = ({
  label,
  description,
  value,
  onChange,
  disabled = false,
  autoFocus,
  "data-testid": testId,
}) => (
  <TextInput
    data-testid={testId}
    label={label}
    description={description}
    value={value}
    onChange={(next) => onChange(normalizeSasInput(next))}
    placeholder={"·".repeat(SAS_LENGTH)}
    disabled={disabled}
    autoFocus={autoFocus}
    autoComplete="off"
    className="font-mono"
  />
);
