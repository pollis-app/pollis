// #1338: once the DS has said this device was removed from the account, the
// core rejects every command with `device_revoked`. Every command passes
// through the native bridge, so that is where it is noticed — whichever
// command happened to hit it first — and the registered handler ends the
// session once, however many in-flight commands fail together. Pure (no
// react-native), so the node tests reach it.

type Handler = () => void | Promise<void>;

let handler: Handler | null = null;
let running = false;
let notice: string | null = null;
const listeners = new Set<() => void>();

export function isDeviceRevokedError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // The core's `Error::DeviceRevoked` displays as exactly `device_revoked`; a
  // command that wraps it appends it after ": ". Anything else that merely
  // mentions the word (a security-event kind, a nudge payload) is not it.
  return message === "device_revoked" || message.endsWith(": device_revoked");
}

/** Register what to do when this device turns out to be revoked. Returns an unsubscribe. */
export function onDeviceRevoked(next: Handler): () => void {
  handler = next;
  return () => {
    if (handler === next) {
      handler = null;
    }
  };
}

/** Called by the bridge with every rejection; runs the handler at most once at a time. */
export function noticeDeviceRevoked(error: unknown): void {
  if (!isDeviceRevokedError(error) || !handler || running) {
    return;
  }
  running = true;
  const run = handler;
  queueMicrotask(() => {
    void Promise.resolve()
      .then(run)
      .catch((e) => console.warn("[device_revoked] handler failed", e))
      .finally(() => {
        running = false;
      });
  });
}

/**
 * Run the handler for a revocation learned some other way (the live
 * `device_revoked` nudge), so every path ends the session the same way.
 */
export function deviceWasRevoked(): void {
  noticeDeviceRevoked("device_revoked");
}

/** Why the last session ended when the user did not end it; shown on sign-in. */
export function getSessionNotice(): string | null {
  return notice;
}

export function setSessionNotice(next: string | null): void {
  notice = next;
  for (const listener of listeners) {
    listener();
  }
}

export function subscribeSessionNotice(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
