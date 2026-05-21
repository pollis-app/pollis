// Native bridge seam. This is the single point where the mobile app talks
// to the Rust core (`pollis-core`) — everything else goes through `invoke()`
// in ./invoke.ts.
//
// CURRENT STATE: placeholder. The `pollis-native` turbo-module
// (`mobile/modules/pollis-native`) is wired into the build but does not yet
// expose a generic `invoke(cmd, args)` entry point. To unblock UI work the
// bridge currently throws a clear "not yet implemented" error for any cmd.
//
// HOW TO PLUG IN REAL BINDINGS: when `pollis-native`'s uniffi-generated
// bindings expose a command dispatcher (e.g. `PollisNative.invoke(cmd, json)`),
// swap `defaultBridge` for an implementation that:
//   1. JSON-stringifies `args`,
//   2. calls the native function across JSI,
//   3. JSON-parses the result,
//   4. throws if the native side returned an error.
//
// Call sites using `invoke<T>(cmd, args)` do not change.

export interface NativeBridge {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

// In-process registry of mock command handlers — useful for early UI work
// and for the typecheck/build to pass without a real native module.
type MockHandler = (args?: Record<string, unknown>) => unknown | Promise<unknown>;
const mockHandlers = new Map<string, MockHandler>();

/**
 * Register a mock implementation for a command. Useful while the real
 * `pollis-native` invoke dispatcher is being built. Returns a disposer.
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
      `[pollis-native] invoke("${cmd}") is not implemented yet. ` +
        `Register a mock via registerMockCommand("${cmd}", …) or wire it ` +
        `through the pollis-native turbo-module.`,
    );
  },
};

let currentBridge: NativeBridge = defaultBridge;

/**
 * Swap the underlying bridge implementation. Production code should call
 * this once at app startup with the real `pollis-native`-backed bridge.
 */
export function setNativeBridge(bridge: NativeBridge): void {
  currentBridge = bridge;
}

export const nativeBridge: NativeBridge = {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return currentBridge.invoke<T>(cmd, args);
  },
};
