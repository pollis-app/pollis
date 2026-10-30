// One run per key at a time (#1610). A request made while a run is in flight shares ONE trailing
// run that starts when it ends, so nothing asked for after a fetch began is missed, and a burst
// of wake-ups (realtime, push, resume, focus) costs two runs, not one each. Pure.

export interface SingleFlight {
  run(key: string, task: () => Promise<void>): Promise<void>;
  /** Keys with a run in flight. For tests. */
  busy(): string[];
}

interface Entry {
  current: Promise<void>;
  trailing: Promise<void> | null;
  // Set when the task begins: a request before that (the same wake-up seen by two listeners
  // in one dispatch) is covered by the run itself and joins it.
  started: boolean;
}

export function createSingleFlight(): SingleFlight {
  const entries = new Map<string, Entry>();

  function start(key: string, task: () => Promise<void>): Promise<void> {
    const entry: Entry = { current: Promise.resolve(), trailing: null, started: false };
    const settle = () => {
      // A trailing run replaces this entry itself; otherwise the key is free again.
      if (entries.get(key) === entry && entry.trailing === null) {
        entries.delete(key);
      }
    };
    entry.current = Promise.resolve()
      .then(() => {
        entry.started = true;
        return task();
      })
      .finally(settle);
    entries.set(key, entry);
    return entry.current;
  }

  function run(key: string, task: () => Promise<void>): Promise<void> {
    const entry = entries.get(key);
    if (!entry) {
      return start(key, task);
    }
    if (!entry.started) {
      return entry.current;
    }
    if (entry.trailing === null) {
      const next = () => start(key, task);
      entry.trailing = entry.current.then(next, next);
    }
    return entry.trailing;
  }

  return {
    run,
    busy: () => [...entries.keys()],
  };
}
