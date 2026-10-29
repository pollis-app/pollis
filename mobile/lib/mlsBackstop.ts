// The re-add backstop mobile runs after a catch-up pass (#1571).
//
// Desktop's cold-launch sweep (`catch_up_all_mls_groups`) ends every pass with
// a reconcile backstop: a device that belongs in a conversation's MLS tree and
// holds no leaf — its KeyPackage pool was empty when it should have been added,
// or its Welcome never applied — is re-added. Mobile never ran it, so in a
// conversation whose participants were all on phones nothing re-added a device
// that was left out, and it found the conversation empty when it opened it.
//
// The bridge's `reconcile_backstop_all` is that backstop on its own. It reads
// each conversation's LOCAL tree, so it runs after the app's ingest pass has
// caught the trees up — landing in the app and every return to the foreground,
// exactly where that pass already runs. One pass at a time: a resume that lands
// while the previous pass is still out does not start a second one.
//
// Pure (no React Native import), so `tests/mls-backstop.test.ts` pins the
// ordering and the single flight.

import type { CatchUpTarget } from "./messageWake";

/** Ingest one conversation; resolves (or rejects) once it is caught up. */
export type IngestOne = (id: string, kind: CatchUpTarget["kind"]) => Promise<unknown>;

/** The bridge call: `reconcile_backstop_all`. */
export type Backstop = () => Promise<unknown>;

let running: Promise<void> | null = null;

/**
 * Catch up every target, then run the backstop once — after ALL of them have
 * settled, whether they succeeded or not (one failed ingest must not cost the
 * rest their re-add). Never throws: both halves are best-effort.
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
