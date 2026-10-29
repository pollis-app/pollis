/** `invoke`, `Channel`, `listen`: real Tauri, or the Playwright mocks in `src/__mocks__/`. */

import {
  invoke as tauriInvoke,
  Channel as TauriChannel,
} from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import { hasTauri } from "./runtime";

type UnlistenFn = () => void;

// Mirrors Tauri's InvokeArgs / InvokeOptions.
export type InvokeArgs =
  | Record<string, unknown>
  | number[]
  | ArrayBuffer
  | Uint8Array;
export interface InvokeOptions {
  headers: HeadersInit;
}

// #1338: every command passes through here, so `device_revoked` is noticed here.
let deviceRevokedHandler: (() => void | Promise<void>) | null = null;
let deviceRevokedRunning = false;

export function isDeviceRevokedError(error: unknown): boolean {
  const message =
    typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  // Exactly `device_revoked`, or suffixed after ": "; mere mentions don't count.
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
  // After the caller's own catch, once per batch of failures.
  queueMicrotask(() => {
    void Promise.resolve()
      .then(handler)
      .catch((e) => console.warn("[device_revoked] handler failed", e))
      .finally(() => {
        deviceRevokedRunning = false;
      });
  });
}

/** Run the revoked handler for a revocation learned elsewhere, so every path ends the same way. */
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

/** Tauri's `Channel<T>` surface; the real Channel under Tauri. */
type ChannelLike<T> = {
  onmessage: (response: T) => void;
  readonly id: number;
};

/** Inert Channel when there is no Tauri host (browser dev, Playwright). */
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

  // Matches Tauri's serialization hook so invoke() can embed the id.
  toJSON(): string {
    return `__CHANNEL__:${this.channelId}`;
  }
}

// Chosen at load: Tauri's Channel registers a callback id in its constructor.
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
