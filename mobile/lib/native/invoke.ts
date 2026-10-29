// Mobile counterpart to desktop's `@tauri-apps/api/core` invoke(), with the
// same signature so call sites port 1:1. See ./bridge.ts.

import { nativeBridge } from "./bridge";

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return nativeBridge.invoke<T>(cmd, args);
}
