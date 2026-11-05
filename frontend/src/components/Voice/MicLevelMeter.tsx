// Mic level bar driven through a ref: ~33 Hz frames paint a transform, never a page re-render.

import React, { useEffect, useRef } from "react";

interface MicLevelMeterProps {
  /** Subscribe to level frames; returns the unsubscribe. */
  subscribe: (listener: (peak: number, rms: number) => void) => () => void;
  label: string;
  testId?: string;
}

export const MicLevelMeter: React.FC<MicLevelMeterProps> = ({ subscribe, label, testId }) => {
  const fill = useRef<HTMLDivElement>(null);
  useEffect(() => {
    return subscribe((peak, rms) => {
      const el = fill.current;
      if (el) {
        el.style.transform = `scaleX(${Math.min(1, Math.max(peak, rms))})`;
      }
    });
  }, [subscribe]);

  return (
    // Reserves its height when idle so start/stop never shifts the layout.
    <div
      data-testid={testId}
      role="meter"
      aria-label={label}
      className="h-3 overflow-hidden rounded bg-surface border border-line"
    >
      <div
        ref={fill}
        data-testid={testId ? `${testId}-fill` : undefined}
        className="h-full w-full origin-left bg-accent will-change-transform transition-transform duration-[60ms] ease-linear"
        style={{ transform: "scaleX(0)" }}
      />
    </div>
  );
};
