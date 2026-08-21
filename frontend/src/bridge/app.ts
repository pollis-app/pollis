/**
 * App / process bridge — version, relaunch, exit.
 *
 * Delegates to `@tauri-apps/api/app` and `@tauri-apps/plugin-process`.
 *
 * There is deliberately no temp-directory accessor. One existed for a single
 * caller, which wrote pasted files into the OS temp directory and never removed
 * them; attachment bytes go to `bridge/staging.ts` instead, and handing the
 * renderer a temp path again would re-open that door — see
 * `frontend/tests/no-plaintext-temp-files.test.ts`.
 *
 * `convertFileSrc` returns a sync string — Tauri's `convertFileSrc` is sync.
 */

export async function getVersion(): Promise<string> {
  const mod = await import("@tauri-apps/api/app");
  return mod.getVersion();
}

export async function relaunch(): Promise<void> {
  const mod = await import("@tauri-apps/plugin-process");
  await mod.relaunch();
}

export async function exit(code = 0): Promise<void> {
  const mod = await import("@tauri-apps/plugin-process");
  await mod.exit(code);
}

// Sync. We eagerly import @tauri-apps/api/core here because the same module
// already underpins invoke/Channel — there's no extra cost.
import { convertFileSrc as tauriConvertFileSrc } from "@tauri-apps/api/core";

export function convertFileSrc(path: string): string {
  return tauriConvertFileSrc(path);
}
