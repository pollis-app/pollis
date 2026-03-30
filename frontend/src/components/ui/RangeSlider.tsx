import React from "react";

interface RangeSliderProps {
  label: string;
  value: number;
  onChange: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  className?: string;
  id?: string;
  sublabel?: string;
  description?: string;
}

export const RangeSlider: React.FC<RangeSliderProps> = ({
  label,
  value,
  onChange,
  min = 0,
  max = 100,
  step = 1,
  disabled = false,
  className = "",
  id,
  sublabel,
  description,
}) => {
  const inputId = id || `slider-${label.toLowerCase().replace(/\s+/g, "-")}`;
  const descriptionId = description ? `${inputId}-description` : undefined;
  const pct = ((value - min) / (max - min)) * 100;

  return (
    <div className={`relative w-full ${className}`}>
      <label
        htmlFor={inputId}
        className="block text-xs font-mono mb-2"
        style={{ color: "var(--c-text-dim)" }}
      >
        {label}
        <span
          className="inline-block ml-2 px-1.5 py-0.5 font-mono font-bold text-xs"
          style={{
            background: "var(--c-active)",
            border: "1px solid var(--c-border-active)",
            borderRadius: 4,
            color: "var(--c-accent)",
          }}
        >
          {value}
        </span>
      </label>

      {sublabel && (
        <p
          className="mb-2 text-xs font-mono"
          style={{ color: "var(--c-text-muted)" }}
        >
          {sublabel}
        </p>
      )}

      <input
        id={inputId}
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        disabled={disabled}
        aria-describedby={descriptionId}
        className="
          w-full h-2 rounded-md appearance-none cursor-pointer
          focus:outline-none focus:ring-4 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black
          disabled:opacity-50 disabled:cursor-not-allowed
          [&::-webkit-slider-thumb]:appearance-none
          [&::-webkit-slider-thumb]:w-3.5
          [&::-webkit-slider-thumb]:h-3.5
          [&::-webkit-slider-thumb]:rounded-full
          [&::-webkit-slider-thumb]:cursor-pointer
          [&::-moz-range-thumb]:w-3.5
          [&::-moz-range-thumb]:h-3.5
          [&::-moz-range-thumb]:rounded-full
          [&::-moz-range-thumb]:border-none
          [&::-moz-range-thumb]:cursor-pointer
        "
        style={{
          background: `linear-gradient(to right, var(--c-accent) 0%, var(--c-accent) ${pct}%, var(--c-border-active) ${pct}%, var(--c-border-active) 100%)`,
          // Thumb color via CSS custom property (pseudo-elements can't use inline styles)
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          ["--thumb-color" as any]: "var(--c-accent)",
        }}
      />

      {description && (
        <p
          id={descriptionId}
          className="mt-1 text-xs font-mono"
          style={{ color: "var(--c-text-muted)" }}
        >
          {description}
        </p>
      )}
    </div>
  );
};
