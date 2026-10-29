// The re-add backstop after a catch-up pass (#1571; .codesight/wiki/commands.md,
// `reconcile_backstop_all`). It reads each conversation's LOCAL tree, so it runs
// after the ingest pass has caught the trees up; one pass at a time. Pure, for
// tests/mls-backstop.test.ts.

import type { CatchUpTarget } from "./messageWake";

/** Ingest one conversation; resolves (or rejects) once it is caught up. */
export type IngestOne = (id: string, kind: CatchUpTarget["kind"]) => Promise<unknown>;

/** The bridge call: `reconcile_backstop_all`. */
export type Backstop = () => Promise<unknown>;

let running: Promise<void> | null = null;

/**
 * Catch up every target, then run the backstop once all have settled: one
 * failed ingest must not cost the rest their re-add. Never throws.
 */
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
