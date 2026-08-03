/**
 * App / path / process bridge — version, temp dir, relaunch, exit.
 *
 * Delegates to `@tauri-apps/api/app`, `@tauri-apps/api/path`, and
 * `@tauri-apps/plugin-process`.
 *
 * `convertFileSrc` returns a sync string — Tauri's `convertFileSrc` is sync.
 */

export async function getVersion(): Promise<string> {
  const mod = await import("@tauri-apps/api/app");
  return mod.getVersion();
}

export async function tempDir(): Promise<string> {
  const mod = await import("@tauri-apps/api/path");
  return mod.tempDir();
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
