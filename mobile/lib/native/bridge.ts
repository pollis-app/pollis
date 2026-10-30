// Native bridge seam: the one place the app talks to the Rust core. Falls back to the mock registry until initializeNativeBridge().

import { Platform } from "react-native";
import * as FileSystem from "expo-file-system/legacy";
import * as pollisNative from "pollis-native";
import { plainBridgeError } from "./bridgeError";
import { noticeDeviceRevoked } from "../deviceRevoked";
import { noticeUpgradeRequired } from "../clientPolicy";

export interface NativeBridge {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

type MockHandler = (args?: Record<string, unknown>) => unknown | Promise<unknown>;
const mockHandlers = new Map<string, MockHandler>();

/** Register a mock for a command; mocks always take precedence. Returns a disposer. */
export function registerMockCommand(cmd: string, handler: MockHandler): () => void {
  mockHandlers.set(cmd, handler);
  return () => {
    mockHandlers.delete(cmd);
  };
}

// Why `initializeNativeBridge` failed, surfaced instead of "no handler".
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

/** Routes invoke() into pollis-core's dispatcher. Registered mocks still win. */
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
      // #1611: a DS 426 blocks the app; the root layout swaps in the update screen.
      noticeUpgradeRequired(failure);
      throw failure;
    }
    if (resultJson === "" || resultJson === "null") {
      return null as T;
    }
    return JSON.parse(resultJson) as T;
  },
};

let currentBridge: NativeBridge = defaultBridge;

/** Swap the underlying bridge (tests). */
export function setNativeBridge(bridge: NativeBridge): void {
  currentBridge = bridge;
}

export interface InitConfig {
  // The client holds NO credential (#987, #393): only non-secret endpoints.
  r2Endpoint?: string;
  r2PublicUrl?: string;
  livekitUrl?: string;
  // No Resend key (#995): reading it made Expo inline it. Never re-add it.
  pollisDeliveryUrl?: string;
  // The dev DS gate key (#1242): dev builds only.
  devAccessKey?: string;
  // #1611: this build's policy key and its NATIVE version (never the OTA bundle's).
  clientPlatform?: string;
  clientVersion?: string | null;
}

let initialized = false;

/**
/** Boot the Rust core and install the JSI bridge. Idempotent; throws if `init_pollis` fails. */
export async function initializeNativeBridge(config: InitConfig): Promise<void> {
  if (initialized) {
    return;
  }
  // Android has no /etc/ssl/certs: Rust writes its CA bundle under `data_dir`.
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
    client_platform: config.clientPlatform ?? null,
    client_version: config.clientVersion ?? null,
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
