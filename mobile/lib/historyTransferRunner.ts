// #1735: drives each unfinished transfer with ONE `run_history_transfer` at a time. Pure (the run is injected).
//
// The run is a single awaited call that returns at a terminal state or "paused" (a bounded idle in Rust). A
// wake-up (unlock, foreground, an `accepted` nudge) that lands while a run is in flight is remembered; if that
// run then comes back paused or still waiting, it runs once more, so a wake-up is never lost and a burst of
// them costs one extra run, not one each. A finished transfer never runs again.

import { isTerminal, type HistoryTransfer } from "./historyTransfer.ts";

export interface TransferRunnerDeps {
  run(transferId: string): Promise<HistoryTransfer>;
  // Mobile's progress stream (uniffi has no event channel): a long poll that answers once the
  // transfer's `updated_at` differs from `since`, or after a bounded wait. Looped while a run is in flight.
  watch?(transferId: string, since: string | null): Promise<HistoryTransfer>;
  onProgress?(transfer: HistoryTransfer): void;
  onResult?(transfer: HistoryTransfer): void;
  onError?(transferId: string, error: unknown): void;
}

export interface TransferRunner {
  kick(transferId: string): Promise<void>;
  /** Kick every unfinished transfer in a freshly read list. */
  kickAll(transfers: readonly HistoryTransfer[]): Promise<void>;
  running(): string[];
}

interface Entry {
  done: Promise<void>;
  again: boolean;
}

export function createTransferRunner(deps: TransferRunnerDeps): TransferRunner {
  const entries = new Map<string, Entry>();

  function start(transferId: string): Promise<void> {
    const entry: Entry = { done: Promise.resolve(), again: false };
    entries.set(transferId, entry);
    entry.done = (async () => {
      for (;;) {
        entry.again = false;
        let result: HistoryTransfer | null = null;
        let settled = false;
        const running = deps.run(transferId).finally(() => {
          settled = true;
        });
        const watching = deps.watch ? follow(transferId, () => settled) : Promise.resolve();
        try {
          result = await running;
          deps.onResult?.(result);
        } catch (e) {
          deps.onError?.(transferId, e);
        }
        // The last long poll ends by its own bound; never start another after the run.
        void watching;
        const finished = result !== null && isTerminal(result.state);
        if (finished || !entry.again) {
          break;
        }
      }
      entries.delete(transferId);
    })();
    return entry.done;
  }

  async function follow(transferId: string, done: () => boolean): Promise<void> {
    let since: string | null = null;
    while (!done()) {
      let snap: HistoryTransfer;
      try {
        snap = await deps.watch!(transferId, since);
      } catch {
        // No retry loop on errors: the run's own result still lands.
        return;
      }
      if (done()) {
        return;
      }
      deps.onProgress?.(snap);
      if (isTerminal(snap.state)) {
        return;
      }
      since = snap.updated_at;
    }
  }

  function kick(transferId: string): Promise<void> {
    const entry = entries.get(transferId);
    if (entry) {
      entry.again = true;
      return entry.done;
    }
    return start(transferId);
  }

  async function kickAll(transfers: readonly HistoryTransfer[]): Promise<void> {
    await Promise.all(
      transfers.filter((t) => !isTerminal(t.state)).map((t) => kick(t.transfer_id)),
    );
  }

  return {
    kick,
    kickAll,
    running: () => [...entries.keys()],
  };
}
