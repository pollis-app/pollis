import React from "react";

export interface RadioOption {
  value: string;
  label: string;
  /** A second, quieter line under the label. */
  detail?: string;
  disabled?: boolean;
}

interface RadioListProps {
  options: RadioOption[];
  value: string | null;
  onChange: (value: string) => void;
  "aria-label": string;
  "data-testid"?: string;
  /** Per-row test id; defaults to `<data-testid>-<value>`. */
  optionTestId?: (option: RadioOption) => string;
}

/** A pick-one choice as a vertical list of filled rows; the chosen row takes the accent fill. Arrow keys move the choice. */
export const RadioList: React.FC<RadioListProps> = ({
  options,
  value,
  onChange,
  "aria-label": ariaLabel,
  "data-testid": testId,
  optionTestId,
}) => {
  const enabled = options.filter((o) => !o.disabled);

  const move = (from: HTMLButtonElement, value: string, step: number) => {
    if (enabled.length === 0) {
      return;
    }
    const at = enabled.findIndex((o) => o.value === value);
    const next = enabled[(at + step + enabled.length) % enabled.length];
    onChange(next.value);
    // Focus follows the choice, as in a native radio group.
    const rows = Array.from(from.parentElement?.querySelectorAll<HTMLButtonElement>("[role=radio]") ?? []);
    rows.find((row) => row.dataset.value === next.value)?.focus();
  };

  return (
    <div role="radiogroup" aria-label={ariaLabel} data-testid={testId} className="flex flex-col gap-1.5">
      {options.map((option) => {
        const selected = option.value === value;
        // Only the chosen row (or the first, before any choice) is in the tab order.
        const tabbable = selected || (value === null && option.value === enabled[0]?.value);
        let rowClass = "bg-surface-raised text-fg enabled:hover:bg-hover";
        if (selected) {
          rowClass = "bg-accent text-bg";
        } else if (option.disabled) {
          rowClass = "bg-surface text-muted cursor-not-allowed";
        }
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            data-value={option.value}
            disabled={option.disabled}
            tabIndex={tabbable ? 0 : -1}
            data-testid={optionTestId ? optionTestId(option) : testId ? `${testId}-${option.value}` : undefined}
            onClick={() => onChange(option.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" || e.key === "ArrowRight") {
                e.preventDefault();
                move(e.currentTarget, option.value, 1);
              } else if (e.key === "ArrowUp" || e.key === "ArrowLeft") {
                e.preventDefault();
                move(e.currentTarget, option.value, -1);
              }
            }}
            className={`flex items-center gap-3 w-full text-start rounded-lg px-4 py-3 border-0 font-mono ${rowClass}`}
          >
            <span
              aria-hidden
              className={`flex items-center justify-center shrink-0 w-4 h-4 rounded-full ${selected ? "bg-bg" : "bg-line-strong"}`}
            >
              {selected && <span className="w-2 h-2 rounded-full bg-accent" />}
            </span>
            <span className="min-w-0 flex flex-col">
              <span className="truncate text-sm">{option.label}</span>
              {option.detail && (
                <span className={`text-xs ${selected ? "text-bg" : "text-dim"}`}>{option.detail}</span>
              )}
            </span>
          </button>
        );
      })}
    </div>
  );
};
