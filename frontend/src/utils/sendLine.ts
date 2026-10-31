// Outgoing messages of one conversation leave in the order they were sent (#1636). Pure.
//
// The composer never waits: every send shows its stub at once and takes a turn here at the tap,
// so a burst of sends is a queue, not a disabled button. Each turn starts when the previous one
// in the same conversation has settled, whether it was delivered or failed, so the core seals
// (and stamps `sent_at`) in tap order. A failure is the failed message's own and holds no one up.

export interface SendTurn {
  /** Resolves when every earlier turn of this conversation has settled. Never rejects. */
  ready: Promise<void>;
  /** Ends this turn. Idempotent: the send's `finally` and its mutation's `onSettled` both call it. */
  release: () => void;
}

export interface SendLine {
  /** Take the next turn in `key`'s line. Call it synchronously at the tap, never after an await. */
  take(key: string): SendTurn;
  /** Keys with a turn taken and not yet released. For tests. */
  busy(): string[];
}

export function createSendLine(): SendLine {
  const tails = new Map<string, Promise<void>>();

  function take(key: string): SendTurn {
    const ready = tails.get(key) ?? Promise.resolve();
    let open!: () => void;
    const done = new Promise<void>((resolve) => {
      open = resolve;
    });
    let released = false;
    const release = () => {
      if (released) {
        return;
      }
      released = true;
      open();
    };
    const tail = ready.then(() => done);
    tails.set(key, tail);
    void tail.then(() => {
      if (tails.get(key) === tail) {
        tails.delete(key);
      }
    });
    return { ready, release };
  }

  return {
    take,
    busy: () => [...tails.keys()],
  };
}
