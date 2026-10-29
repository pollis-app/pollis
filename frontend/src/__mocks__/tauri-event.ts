/** Browser mock for @tauri-apps/api/event; `window.__tauriEmit(name, payload)` delivers to listeners. */

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

/** Deliver `{ payload }`, which `bridge/invoke.ts` unwraps as real Tauri does. */
function deliver(event: string, payload?: unknown): void {
  const set = listeners.get(event);
  if (!set) {
    return;
  }
  for (const handler of [...set]) {
    handler({ payload: payload ?? null });
  }
}

export function emit(event: string, payload?: unknown): Promise<void> {
  deliver(event, payload);
  return Promise.resolve();
}

(window as unknown as Record<string, unknown>).__tauriEmit = deliver;
