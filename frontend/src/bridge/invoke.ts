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

export function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  // Real Tauri runtime, or the Playwright vite-alias mock.
  return tauriInvoke<T>(cmd, args, options);
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
