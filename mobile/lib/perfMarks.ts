// Chat action timings (#2141): tap → visible → confirmed, the rows each action re-rendered and the
// bridge calls it made. Only in a build made with EXPO_PUBLIC_PERF_MARKS=1 (the chat-perf Maestro
// flow reads the summary off the screen); everywhere else every call is a no-op, and a store
// build drops the bodies because the flag is inlined at bundle time.

export const PERF_MARKS = process.env.EXPO_PUBLIC_PERF_MARKS === "1";

export type PerfAction = "send" | "reply" | "edit" | "delete" | "pin" | "react";

/** What has to be on screen before an action counts as visible. */
export type PerfExpect =
  | { kind: "rowText"; text: string }
  | { kind: "rowDeleted"; id: string }
  | { kind: "pinned"; id: string }
  | { kind: "reacted"; id: string; emoji: string }
  | { kind: "editClosed" };

interface Open {
  tap: number;
  waiting: PerfExpect[];
  visible: number | null;
  confirmed: number | null;
  rowsAtTap: number;
  callsAtTap: number;
}

interface Done {
  visible: number[];
  confirmed: number[];
  rows: number[];
  calls: number[];
}

/** How long after it is confirmed an action's renders and calls still count as its own. */
const SETTLE_MS = 600;

const open = new Map<PerfAction, Open>();
const done = new Map<PerfAction, Done>();
let rowRenders = 0;
let bridgeCalls = 0;
const listeners = new Set<() => void>();

function now(): number {
  return globalThis.performance?.now() ?? Date.now();
}

function changed() {
  for (const listener of listeners) {
    listener();
  }
}

/**
 * The splash came down (#2187): logs the JS heap Hermes holds at that point, so a cold-start
 * bench can read what boot allocated off logcat / the device log. A no-op outside a perf build.
 */
export function perfSplashHidden(): void {
  if (!PERF_MARKS) {
    return;
  }
  const hermes = (globalThis as { HermesInternal?: { getInstrumentedStats?: () => Record<string, number> } })
    .HermesInternal;
  const stats = hermes?.getInstrumentedStats?.();
  console.log(
    `[perf] splash-hide js_allocatedBytes=${stats?.js_allocatedBytes ?? -1} js_heapSize=${stats?.js_heapSize ?? -1}`,
  );
}

/** A row render, from the top of MessageRow. */
export function perfRowRender(): void {
  if (PERF_MARKS) {
    rowRenders += 1;
  }
}

/** A bridge call, from `invoke`. */
export function perfBridgeCall(): void {
  if (PERF_MARKS) {
    bridgeCalls += 1;
  }
}

/** The user acted. `waiting` is what must show before the action is visible. */
export function perfTap(action: PerfAction, waiting: PerfExpect[]): void {
  if (!PERF_MARKS) {
    return;
  }
  open.set(action, {
    tap: now(),
    waiting,
    visible: null,
    confirmed: null,
    rowsAtTap: rowRenders,
    callsAtTap: bridgeCalls,
  });
}

function satisfy(test: (e: PerfExpect) => boolean) {
  const at = now();
  for (const mark of open.values()) {
    if (mark.visible !== null) {
      continue;
    }
    mark.waiting = mark.waiting.filter((e) => !test(e));
    if (mark.waiting.length === 0) {
      mark.visible = at;
    }
  }
}

/** A row committed (from its layout effect, so "visible" is the commit that shows it). */
export function perfRowShown(row: {
  id: string;
  text?: string;
  deleted: boolean;
  reacted: (emoji: string) => boolean;
}): void {
  if (!PERF_MARKS || open.size === 0) {
    return;
  }
  satisfy((e) => {
    switch (e.kind) {
      case "rowText":
        return !row.deleted && row.text === e.text;
      case "rowDeleted":
        return row.id === e.id && row.deleted;
      case "reacted":
        return row.id === e.id && row.reacted(e.emoji);
      default:
        return false;
    }
  });
}

/** The conversation's pinned ids committed. */
export function perfPinnedShown(ids: ReadonlySet<string>): void {
  if (PERF_MARKS && open.size > 0) {
    satisfy((e) => e.kind === "pinned" && ids.has(e.id));
  }
}

/** The edit bar is gone. */
export function perfEditClosed(): void {
  if (PERF_MARKS && open.size > 0) {
    satisfy((e) => e.kind === "editClosed");
  }
}

/** The action's mutation settled. */
export function perfConfirmed(action: PerfAction): void {
  if (!PERF_MARKS) {
    return;
  }
  const mark = open.get(action);
  if (!mark || mark.confirmed !== null) {
    return;
  }
  mark.confirmed = now();
  setTimeout(() => {
    if (open.get(action) !== mark) {
      return;
    }
    open.delete(action);
    const d = done.get(action) ?? { visible: [], confirmed: [], rows: [], calls: [] };
    // An action whose change never showed is reported as -1, so a summary never hides it.
    d.visible.push(mark.visible === null ? -1 : mark.visible - mark.tap);
    d.confirmed.push((mark.confirmed ?? mark.tap) - mark.tap);
    d.rows.push(rowRenders - mark.rowsAtTap);
    d.calls.push(bridgeCalls - mark.callsAtTap);
    done.set(action, d);
    changed();
  }, SETTLE_MS);
}

function pct(values: number[], p: number): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1))];
}

/** One line per action, plus the running row-render count (the typing probe reads it). */
export function perfSummary(): string {
  const lines = [`rows=${rowRenders}`];
  for (const [action, d] of done) {
    const r = (n: number) => Math.round(n * 10) / 10;
    lines.push(
      `${action} n=${d.visible.length} vis50=${r(pct(d.visible, 50))} vis95=${r(pct(d.visible, 95))} ` +
        `conf50=${Math.round(pct(d.confirmed, 50))} conf95=${Math.round(pct(d.confirmed, 95))} ` +
        `rows50=${pct(d.rows, 50)} calls50=${pct(d.calls, 50)}`,
    );
  }
  return lines.join("\n");
}

/** Re-render on a new result. */
export function subscribePerf(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
