// #1338: a `device_revoked` rejection from any command ends the session once. Pure.

type Handler = () => void | Promise<void>;

let handler: Handler | null = null;
let running = false;
let notice: string | null = null;
const listeners = new Set<() => void>();

export function isDeviceRevokedError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // Exactly `device_revoked`, or appended after ": "; a mere mention is not it.
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

/** For the live nudge, so every path ends the session the same way. */
export function deviceWasRevoked(): void {
  noticeDeviceRevoked("device_revoked");
}

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
