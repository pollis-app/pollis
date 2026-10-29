/**
 * `invoke`, `Channel`, `listen`: the real Tauri imports, or under Playwright
 * the vite-aliased mocks in `src/__mocks__/`.
 */

import {
  invoke as tauriInvoke,
  Channel as TauriChannel,
} from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import { hasTauri } from "./runtime";

type UnlistenFn = () => void;

// Mirrors Tauri's InvokeArgs / InvokeOptions (raw byte payloads, headers).
export type InvokeArgs =
  | Record<string, unknown>
  | number[]
  | ArrayBuffer
  | Uint8Array;
export interface InvokeOptions {
  headers: HeadersInit;
}

// #1338: once the DS says this device was removed, the core rejects every
// command with `device_revoked`; every command passes through here.
let deviceRevokedHandler: (() => void | Promise<void>) | null = null;
let deviceRevokedRunning = false;

export function isDeviceRevokedError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // `Error::DeviceRevoked` displays as exactly `device_revoked`, or after ": "
  // when wrapped. Other mentions of the word are not it.
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
    // After the failing caller's own catch, and once per batch of failures.
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
 * Run the device-revoked handler for a revocation learned another way (live
 * nudge, startup check), so every path ends the session the same way.
 */
export function deviceWasRevoked(): void {
  noticeDeviceRevoked("device_revoked");
}

export function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
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

/** Tauri's `Channel<T>` surface; the real class serializes via `SERIALIZE_TO_IPC_FN`. */
type ChannelLike<T> = {
  onmessage: (response: T) => void;
  readonly id: number;
};

/**
 * Inert stand-in when no Tauri host is present (`pnpm dev:frontend`,
 * Playwright): constructing a `Channel` must not throw, and nothing pushes.
 */
class StubChannel<T = unknown> implements ChannelLike<T> {
  readonly id: number;
  readonly channelId: string;
  #handler: (response: T) => void = () => {};

  constructor() {
    this.channelId = makeStubChannelId();
    this.id = nextStubChannelId;
  }

  set onmessage(handler: (response: T) => void) {
    this.#handler = handler;
  }

  get onmessage(): (response: T) => void {
    return this.#handler;
  }

  toJSON(): string {
    return `__CHANNEL__:${this.channelId}`;
  }
}

// Chosen at module load: Tauri's Channel registers a callback id in its
// constructor, which needs the runtime present.
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
  return tauriListen<T>(event, (e) => handler(e.payload));
}
