// Mobile counterpart to desktop's `@tauri-apps/api/core` invoke().
//
// Signature is intentionally identical so call sites can be ported 1:1:
//
//   import { invoke } from "../../lib/native/invoke";
//   const groups = await invoke<Group[]>("list_user_groups");
//
// This routes through `nativeBridge` (see ./bridge.ts), which in production
// calls the real `pollis-native` JSI module (uniffi-bindgen-react-native) and
// lands in the Rust dispatcher at `pollis-core/src/bridge.rs`. Before
// `initializeNativeBridge()` runs — and in jest / typecheck — it falls back to
// the `registerMockCommand` registry.

import { nativeBridge } from "./bridge";

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return nativeBridge.invoke<T>(cmd, args);
}
