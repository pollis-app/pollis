/**
 * Test-only render accounting (#874).
 *
 * Render cost is the kind of claim that is easy to assert and hard to prove. A
 * stopwatch measures the machine it ran on; this measures the thing we actually
 * changed — how many times a component's render function is entered.
 *
 * `probeRender` is called at the TOP of a component body rather than from an
 * effect, on purpose: when `React.memo` (which `observer()` applies for us)
 * bails a component out, the render function is never entered at all, so the
 * counter is exactly the signal we want. An effect would work too, but only
 * measures committed renders and adds a per-row effect to ship.
 *
 * Cost in a real build is zero. `import.meta.env.VITE_PLAYWRIGHT` is statically
 * replaced by Vite, so `PROBE_ENABLED` is the literal `false` everywhere except
 * the Playwright build and Rollup drops the body and the `window` global with
 * it. Nothing here is reachable from a shipped binary.
 *
 * Note for anyone reading counts: the e2e build runs under `React.StrictMode`,
 * which double-invokes render in dev. Absolute numbers are therefore 2x. The
 * assertions that matter are relative ("this interaction added no renders"), so
 * the factor cancels — never hard-code an expected absolute count.
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
