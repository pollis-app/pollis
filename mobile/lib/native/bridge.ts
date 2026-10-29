// Native bridge seam: the one place the app talks to `pollis-core`; everything
// else goes through `invoke()` in ./invoke.ts. `initializeNativeBridge()` (from
// app/_layout.tsx) swaps the mock-only `defaultBridge` for the JSI-backed one.

import { Platform } from "react-native";
import * as FileSystem from "expo-file-system/legacy";
import * as pollisNative from "pollis-native";
import { plainBridgeError } from "./bridgeError";
import { noticeDeviceRevoked } from "../deviceRevoked";

export interface NativeBridge {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

type MockHandler = (args?: Record<string, unknown>) => unknown | Promise<unknown>;
const mockHandlers = new Map<string, MockHandler>();

/**
 * Register a mock for a command. Mocks take precedence over any bridge, the
 * real one included. Returns a disposer.
 */
export function registerMockCommand(cmd: string, handler: MockHandler): () => void {
  mockHandlers.set(cmd, handler);
  return () => {
    mockHandlers.delete(cmd);
  };
}

// Why `initializeNativeBridge` failed, if it did: what the caller needs to see
// instead of "no handler".
let initFailure: string | null = null;

const defaultBridge: NativeBridge = {
  async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const mock = mockHandlers.get(cmd);
    if (mock) {
      return (await mock(args)) as T;
    }
    if (initFailure) {
      throw new Error(`[pollis-native] the core did not start: ${initFailure}`);
    }
    throw new Error(
      `[pollis-native] invoke("${cmd}") has no handler. ` +
        `Call initializeNativeBridge() at app startup, or register a mock via ` +
        `registerMockCommand("${cmd}", …) for development.`,
    );
  },
};

/** Routes invoke() into the Rust dispatcher in `pollis-core/src/bridge.rs`. */
const pollisNativeBridge: NativeBridge = {
  async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const mock = mockHandlers.get(cmd);
    if (mock) {
      return (await mock(args)) as T;
    }
    const argsJson = args ? JSON.stringify(args) : "{}";
    let resultJson: string;
    try {
      resultJson = await pollisNative.invoke(cmd, argsJson);
    } catch (err) {
      const failure = plainBridgeError(err);
      noticeDeviceRevoked(failure);
      throw failure;
    }
    if (resultJson === "" || resultJson === "null") {
      return null as T;
    }
    return JSON.parse(resultJson) as T;
  },
};

let currentBridge: NativeBridge = defaultBridge;

/** For tests; `initializeNativeBridge()` is the normal entry point. */
export function setNativeBridge(bridge: NativeBridge): void {
  currentBridge = bridge;
}

export interface InitConfig {
  // No database, R2, LiveKit or Resend credential (#393, #987, #995): the client
  // holds no secret, and anything read from an `EXPO_PUBLIC_*` var here is
  // inlined into the bundle. Never add one.
  r2Endpoint?: string;
  r2PublicUrl?: string;
  livekitUrl?: string;
  // The only backend since #987. Optional in the type only because Rust defaults
  // it; without it the app can open its local database and nothing else.
  pollisDeliveryUrl?: string;
  // The dev DS gate key (#1242): dev builds only, sent by Rust only to a
  // non-prod DS. A store, APK or OTA build refuses to configure with it set.
  devAccessKey?: string;
}

let initialized = false;

/**
 * Boot the Rust core and install the JSI-backed bridge. Idempotent; call before
 * any `invoke()` consumer mounts. Throws if `init_pollis` fails.
 */
export async function initializeNativeBridge(config: InitConfig): Promise<void> {
  if (initialized) {
    return;
  }
  // Android has no /etc/ssl/certs, so Rust writes its bundled Mozilla CA file
  // under `data_dir` and needs a writable path (scheme stripped for PathBuf).
  const dataDir =
    Platform.OS === "android" && FileSystem.documentDirectory
      ? FileSystem.documentDirectory.replace(/^file:\/\//, "")
      : undefined;
  const configJson = JSON.stringify({
    data_dir: dataDir,
    r2_endpoint: config.r2Endpoint ?? "",
    r2_public_url: config.r2PublicUrl ?? "",
    livekit_url: config.livekitUrl ?? "",
    pollis_delivery_url: config.pollisDeliveryUrl ?? "",
    dev_access_key: config.devAccessKey ?? "",
  });
  try {
    await pollisNative.initPollis(configJson);
  } catch (err) {
    const failure = plainBridgeError(err);
    initFailure = failure instanceof Error ? failure.message : String(failure);
    throw failure;
  }
  initFailure = null;
  setNativeBridge(pollisNativeBridge);
  initialized = true;
}

export const nativeBridge: NativeBridge = {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return currentBridge.invoke<T>(cmd, args);
  },
};
