/**
 * Browser-side mock for @tauri-apps/api/event used when VITE_PLAYWRIGHT=true.
 *
 * Listeners used to be dropped on the floor, which made every backend-pushed
 * event untestable in the browser tier. They are now kept in a registry and
 * `window.__tauriEmit(name, payload)` delivers to them, so a spec can drive a
 * Rust-originated event (the idle auto-lock, #851) the same way the real shell
 * would. Registering a handler is still side-effect-free, so existing specs
 * that never emit behave exactly as before.
 */

type UnlistenFn = () => void;
type Handler = (event: { payload: unknown }) => void;

const listeners = new Map<string, Set<Handler>>();

function add(event: string, handler: Handler): UnlistenFn {
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  set.add(handler);
  return () => {
    set?.delete(handler);
  };
}

export function listen(
  event: string,
  handler: (event: { payload: unknown }) => void,
): Promise<UnlistenFn> {
  return Promise.resolve(add(event, handler));
}

export function once(
  event: string,
  handler: (event: { payload: unknown }) => void,
): Promise<UnlistenFn> {
  const unlisten = add(event, (e) => {
    unlisten();
    handler(e);
  });
  return Promise.resolve(unlisten);
}

/**
 * Deliver an event to everything currently listening. Note the
 * `{ payload }` envelope: `bridge/invoke.ts` unwraps `e.payload` before
 * handing the value to app code, exactly as real Tauri does.
 */
function deliver(event: string, payload?: unknown): void {
  const set = listeners.get(event);
  if (!set) {
    return;
  }
  // Snapshot: a handler is allowed to unlisten itself (see `once`).
  for (const handler of [...set]) {
    handler({ payload: payload ?? null });
  }
}

export function emit(event: string, payload?: unknown): Promise<void> {
  deliver(event, payload);
  return Promise.resolve();
}

// The test-side entry point. Specs call this from `page.evaluate` to stand in
// for the Rust shell's `AppHandle::emit`.
(window as unknown as Record<string, unknown>).__tauriEmit = deliver;
