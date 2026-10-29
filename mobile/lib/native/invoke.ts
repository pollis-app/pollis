// Mobile counterpart to desktop's invoke(), same signature; routes through ./bridge.ts.

import { nativeBridge } from "./bridge";

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return nativeBridge.invoke<T>(cmd, args);
}
