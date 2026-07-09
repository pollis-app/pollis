// Native bridge seam. This is the single point where the mobile app talks
// to the Rust core (`pollis-core`) — everything else goes through `invoke()`
// in ./invoke.ts.
//
// Two implementations live behind the same NativeBridge interface:
//
//   1. `pollisNativeBridge` (default in production) — calls into the
//      `pollis-native` JSI turbo-module. JSON-marshals args, awaits the
//      Rust `invoke()` promise, JSON-parses the result. Call sites that
//      use `invoke<T>(cmd, args)` get fully-typed results back.
//
//   2. `defaultBridge` (fallback) — used when neither the real native
//      module nor an explicit override has been installed. Defers to the
//      `registerMockCommand` registry, and throws a clear error for any
//      unregistered command. Useful for jest / typecheck / unit work.
//
// Production code calls `initializeNativeBridge()` once at app startup
// (see app/_layout.tsx) which calls Rust's `initPollis()` with config
// and then swaps in `pollisNativeBridge`.

import { Platform } from "react-native";
import * as FileSystem from "expo-file-system/legacy";
import * as pollisNative from "pollis-native";

export interface NativeBridge {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

type MockHandler = (args?: Record<string, unknown>) => unknown | Promise<unknown>;
const mockHandlers = new Map<string, MockHandler>();

/**
 * Register a mock implementation for a command. Mocks always take precedence
 * over the underlying bridge — handy for screen-development without a real
 * backend, or for jest tests that don't want to spin up Turso. Returns a
 * disposer.
 */
export function registerMockCommand(cmd: string, handler: MockHandler): () => void {
  mockHandlers.set(cmd, handler);
  return () => {
    mockHandlers.delete(cmd);
  };
}

const defaultBridge: NativeBridge = {
  async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const mock = mockHandlers.get(cmd);
    if (mock) {
      return (await mock(args)) as T;
    }
    throw new Error(
      `[pollis-native] invoke("${cmd}") has no handler. ` +
        `Call initializeNativeBridge() at app startup, or register a mock via ` +
        `registerMockCommand("${cmd}", …) for development.`,
    );
  },
};

/**
 * Production bridge — routes invoke() through `pollis-native`'s JSI module
 * into the Rust dispatcher in `pollis-core/src/bridge.rs`. Mocks still win
 * if registered, so individual commands can be stubbed during UI work even
 * after the bridge is installed.
 */
const pollisNativeBridge: NativeBridge = {
  async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const mock = mockHandlers.get(cmd);
    if (mock) {
      return (await mock(args)) as T;
    }
    const argsJson = args ? JSON.stringify(args) : "{}";
    const resultJson = await pollisNative.invoke(cmd, argsJson);
    if (resultJson === "" || resultJson === "null") {
      return null as T;
    }
    return JSON.parse(resultJson) as T;
  },
};

let currentBridge: NativeBridge = defaultBridge;

/**
 * Swap the underlying bridge implementation. Most call sites should not
 * touch this — `initializeNativeBridge()` is the normal entry point. Exposed
 * for tests that want to inject a fully custom bridge.
 */
export function setNativeBridge(bridge: NativeBridge): void {
  currentBridge = bridge;
}

export interface InitConfig {
  tursoUrl: string;
  tursoToken: string;
  // R2 access credentials moved server-side to the DS secrets broker (#393); the
  // bundle no longer carries them. Only the non-secret endpoint/public URL remain.
  r2Endpoint?: string;
  r2PublicUrl?: string;
  // LiveKit API key/secret moved server-side to the DS broker (#393); the bundle
  // no longer carries them. Only the non-secret ws URL remains (the SFU dial URL,
  // also returned by the DS token endpoint).
  livekitUrl?: string;
  resendApiKey?: string;
  // Delivery Service base URL (api-dev.pollis.com / api.pollis.com). Required —
  // OTP bootstrap and every remote write go through the DS, not direct Turso.
  pollisDeliveryUrl?: string;
}

let initialized = false;

/**
 * Boot the Rust core and install the JSI-backed bridge. Idempotent — safe to
 * call multiple times; subsequent calls are no-ops. Call once during app
 * startup before any `invoke()` consumer mounts (the root layout in
 * `app/_layout.tsx` is the right place).
 *
 * Throws if `init_pollis` on the Rust side fails (e.g. Turso URL is
 * unreachable, config JSON is malformed).
 */
export async function initializeNativeBridge(config: InitConfig): Promise<void> {
  if (initialized) {
    return;
  }
  // Android's rustls-native-certs (pulled transitively by libsql) reads
  // /etc/ssl/certs which is empty on Android. Rust ships a Mozilla CA
  // bundle and writes it under `data_dir`; we need to hand it a writable
  // path. Expo's documentDirectory comes back as a `file://` URL — strip
  // the scheme for Rust's PathBuf.
  const dataDir =
    Platform.OS === "android" && FileSystem.documentDirectory
      ? FileSystem.documentDirectory.replace(/^file:\/\//, "")
      : undefined;
  const configJson = JSON.stringify({
    turso_url: config.tursoUrl,
    turso_token: config.tursoToken,
    data_dir: dataDir,
    r2_endpoint: config.r2Endpoint ?? "",
    r2_public_url: config.r2PublicUrl ?? "",
    livekit_url: config.livekitUrl ?? "",
    resend_api_key: config.resendApiKey ?? "",
    pollis_delivery_url: config.pollisDeliveryUrl ?? "",
  });
  await pollisNative.initPollis(configJson);
  setNativeBridge(pollisNativeBridge);
  initialized = true;
}

export const nativeBridge: NativeBridge = {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return currentBridge.invoke<T>(cmd, args);
  },
};
