/** Test-only render counts (#874), compiled out unless VITE_PLAYWRIGHT. Call at the top of a render; StrictMode doubles counts. */

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
