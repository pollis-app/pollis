/**
 * Test-only render accounting (#874): counts render-function entries, called
 * at the TOP of a component so a memo bail-out counts zero. Compiled out of
 * real builds (`VITE_PLAYWRIGHT`). The e2e build runs under StrictMode, so
 * counts are 2x: assert relative changes, never absolute counts.
 */

const PROBE_ENABLED = import.meta.env.VITE_PLAYWRIGHT === "true";

interface ProbeWindow {
  __pollisRenders?: Record<string, number>;
}

export function probeRender(name: string): void {
  if (!PROBE_ENABLED) {
    return;
  }
  const w = window as unknown as ProbeWindow;
  if (!w.__pollisRenders) {
    w.__pollisRenders = {};
  }
  w.__pollisRenders[name] = (w.__pollisRenders[name] ?? 0) + 1;
}
