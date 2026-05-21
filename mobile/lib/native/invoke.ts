// Mobile counterpart to desktop's `@tauri-apps/api/core` invoke().
//
// Signature is intentionally identical so call sites can be ported 1:1:
//
//   import { invoke } from "../../lib/native/invoke";
//   const groups = await invoke<Group[]>("list_user_groups");
//
// Today this routes through `nativeBridge` (see ./bridge.ts) which is a
// placeholder turbomodule seam. Real bindings will come from the
// `pollis-native` JSI module (uniffi-bindgen-react-native) — when those
// land, only `./bridge.ts` needs to swap implementations. Call sites that
// already use `invoke(cmd, args)` keep working unchanged.

import { nativeBridge } from "./bridge";

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return nativeBridge.invoke<T>(cmd, args);
}
