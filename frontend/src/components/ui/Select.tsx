import React, { useId } from "react";
import { ChevronDown } from "lucide-react";
import { FOCUS_RING } from "./focusRing";

interface SelectOption<T extends string> {
  value: T;
  label: string;
}

interface SelectProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: SelectOption<T>[];
  /** Shown above the control and tied to it, so it is the control's accessible name. */
  label: string;
  disabled?: boolean;
  "data-testid"?: string;
}

/**
 * A pick-one dropdown (#1964). Drawn like `TextInput`: the UI face (mono only in Terminal),
 * `border-line-strong` edge turning `accent` on focus, the per-skin `rounded-control` corner
 * and the shared focus ring. The caret sits at the end edge, so it flips under `dir=rtl`.
 */
export function Select<T extends string>({ value, onChange, options, label, disabled, "data-testid": testId }: SelectProps<T>) {
  const id = useId();
  return (
    <div className="flex flex-col gap-1 max-w-[21rem]">
      <label htmlFor={id} className="text-muted">
        {label}
      </label>
      <div className="relative">
        <select
          id={id}
          value={value}
          disabled={disabled}
          data-testid={testId}
          onChange={(e) => onChange(e.target.value as T)}
          className={`w-full appearance-none cursor-pointer py-1.5 ps-2 pe-7 font-mono text-body rounded-control bg-surface text-fg border-2 border-line-strong focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed ${FOCUS_RING}`}
        >
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        <ChevronDown
          size={14}
          aria-hidden="true"
          className="absolute end-2 top-1/2 -translate-y-1/2 pointer-events-none text-muted"
        />
      </div>
    </div>
  );
}
