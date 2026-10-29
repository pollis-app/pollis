// Holds a notification tap until the device is past the PIN (#1422); newest wins. Import-free.

export interface TapGate<T> {
  /** Open now if ready, else hold it. */
  tap(target: T): void;
  setReady(ready: boolean): void;
  clear(): void;
}

export function createTapGate<T>(open: (target: T) => void): TapGate<T> {
  let ready = false;
  let pending: { target: T } | null = null;
  return {
    tap(target) {
      if (ready) {
        open(target);
        return;
      }
      pending = { target };
    },
    setReady(next) {
      ready = next;
      if (!ready || pending === null) {
        return;
      }
      const held = pending.target;
      pending = null;
      open(held);
    },
    clear() {
      pending = null;
    },
  };
}
