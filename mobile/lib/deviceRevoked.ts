// #1338: a revoked device's core rejects every command with `device_revoked`.
// The native bridge notices it on whichever command fails first, and the
// handler ends the session once, however many commands fail together. Pure, so
// the node tests reach it.

type Handler = () => void | Promise<void>;

let handler: Handler | null = null;
let running = false;
let notice: string | null = null;
const listeners = new Set<() => void>();

export function isDeviceRevokedError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // `Error::DeviceRevoked` displays as exactly `device_revoked`, or after ": "
  // when wrapped. Text that merely mentions the word is not it.
  return message === "device_revoked" || message.endsWith(": device_revoked");
}

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

/** For a revocation learned another way (the live nudge), so every path ends the session the same. */
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
