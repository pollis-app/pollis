// Holds a notification tap until the device is in the signed-in app (#1422),
// so a cold-start or locked-app tap is neither dropped nor pushed over the PIN
// screen against a closed database. Newest tap wins. No imports (node tests).

export interface TapGate<T> {
  /** A tap arrived: open it now if ready, else hold it (newest wins). */
  tap(target: T): void;
  /** The app became ready (or stopped being). Becoming ready opens the held tap. */
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
