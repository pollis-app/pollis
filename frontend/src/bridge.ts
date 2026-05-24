/**
 * Runtime bridge for invoke / Channel / listen.
 *
 * During the Tauri → Electron migration, the React frontend must run
 * unchanged under either runtime. This module detects which host is
 * present at call time and routes through it.
 *
 * Detection:
 *  - Electron: a preload script exposes `window.electronAPI`. When present,
 *    we route everything through it.
 *  - Otherwise: fall through to `@tauri-apps/api`. Under the real Tauri
 *    runtime this hits the webview's IPC; under Playwright the vite alias
 *    swaps in `__mocks__/tauri-core.ts`.
 */

import {
  invoke as tauriInvoke,
  Channel as TauriChannel,
} from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

type UnlistenFn = () => void;

// Mirrors Tauri's InvokeArgs / InvokeOptions so callers that pass raw
// byte payloads (e.g. terminal_write) or per-call HTTP headers keep
// compiling. Electron's preload only needs to forward these along.
export type InvokeArgs =
  | Record<string, unknown>
  | number[]
  | ArrayBuffer
  | Uint8Array;
export interface InvokeOptions {
  headers: HeadersInit;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
    electronAPI?: {
      invoke: <T>(
        cmd: string,
        args?: InvokeArgs,
        options?: InvokeOptions,
      ) => Promise<T>;
      on: (event: string, handler: (payload: unknown) => void) => UnlistenFn;
      channelOn: (id: string, handler: (payload: unknown) => void) => UnlistenFn;
    };
  }
}

function hasElectron(): boolean {
  return typeof window !== "undefined" && window.electronAPI !== undefined;
}

function hasTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as Window).__TAURI_INTERNALS__ !== undefined
  );
}

export function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  if (hasElectron()) {
    return window.electronAPI!.invoke<T>(cmd, args, options);
  }
  // Real Tauri runtime, or the Playwright vite-alias mock.
  return tauriInvoke<T>(cmd, args, options);
}

let nextElectronChannelId = 0;
function makeElectronChannelId(): string {
  nextElectronChannelId += 1;
  return `bridge-channel-${nextElectronChannelId}-${Date.now()}`;
}

/**
 * Channel API surface compatible with Tauri's `Channel<T>`.
 *
 * Under Tauri, this is the real Tauri Channel (re-exported as-is) so that
 * `invoke` can serialize it through its `SERIALIZE_TO_IPC_FN` hook and the
 * backend can route messages by numeric id.
 *
 * Under Electron, this is a polyfill that registers a string-id IPC
 * listener via the preload bridge and serializes itself to that id when
 * passed as an argument to `invoke`.
 */
type ChannelLike<T> = {
  onmessage: (response: T) => void;
  readonly id: number;
};

class ElectronChannel<T = unknown> implements ChannelLike<T> {
  readonly id: number;
  readonly channelId: string;
  #handler: (response: T) => void = () => {};
  #unsubscribe: UnlistenFn | null = null;

  constructor() {
    this.channelId = makeElectronChannelId();
    // Surface a numeric id for API compatibility. Electron routes by the
    // string channelId, so the numeric value is informational only.
    this.id = nextElectronChannelId;
  }

  set onmessage(handler: (response: T) => void) {
    this.#handler = handler;
    if (this.#unsubscribe) {
      this.#unsubscribe();
    }
    if (typeof window !== "undefined" && window.electronAPI) {
      this.#unsubscribe = window.electronAPI.channelOn(
        this.channelId,
        (payload) => this.#handler(payload as T),
      );
    }
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
// Under Electron (or when no host is present), use the polyfill.
const ChannelImpl: new <T>() => ChannelLike<T> =
  !hasElectron() && TauriChannel !== undefined && hasTauri()
    ? (TauriChannel as unknown as new <T>() => ChannelLike<T>)
    : (ElectronChannel as unknown as new <T>() => ChannelLike<T>);

export const Channel = ChannelImpl;
export type Channel<T> = ChannelLike<T>;

export function listen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  if (hasElectron()) {
    const unlisten = window.electronAPI!.on(event, (payload) =>
      handler(payload as T),
    );
    return Promise.resolve(unlisten);
  }
  // Real Tauri, or the Playwright vite-alias mock (which returns a noop).
  return tauriListen<T>(event, (e) => handler(e.payload));
}
