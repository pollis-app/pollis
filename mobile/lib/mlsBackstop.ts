// The MLS re-add backstop (#1571), run after each catch-up pass's ingests settle; single flight. Pure.

import type { CatchUpTarget } from "./messageWake";

export type IngestOne = (id: string, kind: CatchUpTarget["kind"]) => Promise<unknown>;

export type Backstop = () => Promise<unknown>;

let running: Promise<void> | null = null;

/** Runs the backstop once ALL targets settled, succeeded or not. Never throws. */
export function catchUpThenBackstop(
  targets: ReadonlyArray<CatchUpTarget>,
  ingest: IngestOne,
  backstop: Backstop,
): Promise<void> {
  if (running) {
    return running;
  }
  running = (async () => {
    await Promise.allSettled(targets.map((t) => ingest(t.id, t.kind)));
    try {
      await backstop();
    } catch (e) {
      console.warn("[mls-backstop] reconcile_backstop_all failed:", e);
    }
  })().finally(() => {
    running = null;
  });
  return running;
}
