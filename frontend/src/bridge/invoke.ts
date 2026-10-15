/**
 * `invoke`, `Channel`, `listen` — the original three-symbol Tauri surface.
 *
 * Routes to the real `@tauri-apps/api/core` / `event` imports, or — under
 * Playwright — to the vite-aliased mocks in `src/__mocks__/`.
 */

import {
  invoke as tauriInvoke,
  Channel as TauriChannel,
} from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import { hasTauri } from "./runtime";

type UnlistenFn = () => void;

// Mirrors Tauri's InvokeArgs / InvokeOptions so callers that pass raw
// byte payloads (e.g. terminal_write) or per-call HTTP headers keep
// compiling.
export type InvokeArgs =
  | Record<string, unknown>
  | number[]
  | ArrayBuffer
  | Uint8Array;
export interface InvokeOptions {
  headers: HeadersInit;
}

// #1338: the core rejects every command with `device_revoked` once the DS has
// said this device was removed from the account. The one place every command
// passes through is here, so this is where it is noticed, whichever command
// happened to hit it first.
let deviceRevokedHandler: (() => void | Promise<void>) | null = null;
let deviceRevokedRunning = false;

export function isDeviceRevokedError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // The core's `Error::DeviceRevoked` displays as exactly `device_revoked`; a
  // command that wraps it appends it after ": ". Anything else that merely
  // mentions the word (a security-event kind, a nudge payload) is not it.
  return message === "device_revoked" || message.endsWith(": device_revoked");
}

/** Register what to do when this device turns out to be revoked. Returns an unsubscribe. */
export function onDeviceRevoked(handler: () => void | Promise<void>): () => void {
  deviceRevokedHandler = handler;
  return () => {
    if (deviceRevokedHandler === handler) {
      deviceRevokedHandler = null;
    }
  };
}

function noticeDeviceRevoked(error: unknown): void {
  if (!isDeviceRevokedError(error) || !deviceRevokedHandler || deviceRevokedRunning) {
    return;
  }
  deviceRevokedRunning = true;
  const handler = deviceRevokedHandler;
  // Run after the failing caller's own catch, and only once however many
  // in-flight commands fail together.
  queueMicrotask(() => {
    void Promise.resolve()
      .then(handler)
      .catch((e) => console.warn("[device_revoked] handler failed", e))
      .finally(() => {
        deviceRevokedRunning = false;
      });
  });
}

/**
 * Run the device-revoked handler for a revocation learned some other way (the
 * live `device_revoked` nudge, the startup registration check), so every path
 * ends the session the same way and shows the same notice.
 */
export function deviceWasRevoked(): void {
  noticeDeviceRevoked("device_revoked");
}

export function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  // Real Tauri runtime, or the Playwright vite-alias mock.
  return tauriInvoke<T>(cmd, args, options).catch((error: unknown) => {
    noticeDeviceRevoked(error);
    throw error;
  });
}

let nextStubChannelId = 0;
function makeStubChannelId(): string {
  nextStubChannelId += 1;
  return `bridge-channel-${nextStubChannelId}-${Date.now()}`;
}

/**
 * Channel API surface compatible with Tauri's `Channel<T>`.
 *
 * Under Tauri, this is the real Tauri Channel (re-exported as-is) so that
 * `invoke` can serialize it through its `SERIALIZE_TO_IPC_FN` hook and the
 * backend can route messages by numeric id.
 */
type ChannelLike<T> = {
  onmessage: (response: T) => void;
  readonly id: number;
};

/**
 * Inert stand-in used when no Tauri host is present — browser-only dev
 * (`pnpm dev:frontend`) and Playwright, where `@tauri-apps/api/core` is
 * vite-aliased to a mock that exports no `Channel`. It stores the handler and
 * is never fed messages, which is what those environments want: constructing a
 * `Channel` must not throw, and there is no backend to push events from.
 */
class StubChannel<T = unknown> implements ChannelLike<T> {
  readonly id: number;
  readonly channelId: string;
  #handler: (response: T) => void = () => {};

  constructor() {
    this.channelId = makeStubChannelId();
    // Surface a numeric id for API compatibility.
    this.id = nextStubChannelId;
  }

  set onmessage(handler: (response: T) => void) {
    this.#handler = handler;
  }

  get onmessage(): (response: T) => void {
    return this.#handler;
  }

  // Matches Tauri's serialization hook so invoke() can embed the id.
  toJSON(): string {
    return `__CHANNEL__:${this.channelId}`;
  }
}

// Pick the correct concrete class at module load. We can't switch at
// `new`-time because Tauri's Channel auto-registers a numeric callback id
// in its constructor, which must run when the Tauri runtime is present.
// When no host is present, use the inert stub.
const ChannelImpl: new <T>() => ChannelLike<T> =
  TauriChannel !== undefined && hasTauri()
    ? (TauriChannel as unknown as new <T>() => ChannelLike<T>)
    : (StubChannel as unknown as new <T>() => ChannelLike<T>);

export const Channel = ChannelImpl;
export type Channel<T> = ChannelLike<T>;

export function listen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  // Real Tauri, or the Playwright vite-alias mock (which returns a noop).
  return tauriListen<T>(event, (e) => handler(e.payload));
}
