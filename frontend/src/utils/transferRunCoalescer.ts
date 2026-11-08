// #1735: one run per transfer per window, and no lost wake-up. A wake-up (a nudge, an unlock) that lands
// while a run is in flight is remembered; if that run comes back paused or still waiting, it runs once
// more — so a nudge arriving just as Rust decides to pause is never dropped, and a burst of nudges costs
// one extra run, not one each. A finished transfer never runs again. Mirrors mobile's
// `lib/historyTransferRunner.ts`. Pure (no bridge), so it is unit-tested.

export interface RunCoalescer<T> {
  /** Start a run, or join the one in flight (and ask it to run once more when it ends). */
  run(id: string): Promise<T | null>;
  isRunning(id: string): boolean;
}

interface Entry<T> {
  again: boolean;
  done: Promise<T | null>;
}

export function createRunCoalescer<T>(
  runOnce: (id: string) => Promise<T | null>,
  isFinished: (result: T) => boolean,
): RunCoalescer<T> {
  const entries = new Map<string, Entry<T>>();
  return {
    run(id) {
      const existing = entries.get(id);
      if (existing) {
        existing.again = true;
        return existing.done;
      }
      const entry: Entry<T> = { again: false, done: Promise.resolve(null) };
      entries.set(id, entry);
      entry.done = (async () => {
        let result: T | null = null;
        try {
          for (;;) {
            entry.again = false;
            result = await runOnce(id);
            if ((result !== null && isFinished(result)) || !entry.again) {
              return result;
            }
          }
        } finally {
          entries.delete(id);
        }
      })();
      return entry.done;
    },
    isRunning(id) {
      return entries.has(id);
    },
  };
}
