// Holds a notification tap until the app can open a conversation (#1422).
//
// A tap can land before the app is ready to route it: on a cold start the
// response fires while the session is still being restored and the PIN is not
// entered yet, and on a warm start the app may be auto-locked. Routing then
// either dropped the tap (no listener installed yet) or pushed the
// conversation over the PIN screen against a closed database. The gate keeps
// the newest tap and opens it once the device is in the signed-in app.
//
// No imports and no I/O, so `tests/push-tap-gate.test.ts` runs it under node.

export interface TapGate<T> {
  /** A tap arrived: open it now if ready, else hold it (newest wins). */
  tap(target: T): void;
  /** The app became ready (or stopped being). Becoming ready opens the held tap. */
  setReady(ready: boolean): void;
  /** Drop any held tap, e.g. on sign-out. */
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
