/**
 * App / path / process bridge — version, temp dir, relaunch, exit.
 *
 * Under Tauri: delegates to `@tauri-apps/api/app`, `@tauri-apps/api/path`,
 * and `@tauri-apps/plugin-process`. Under Electron: routes to the preload
 * IPC handlers.
 *
 * convertFileSrc returns a sync string in both runtimes — Tauri's
 * `convertFileSrc` is sync, and Electron's preload exposes a sync wrapper
 * that just builds the `pollis-file://<encoded>` URL.
 */

import { electron, hasElectron } from "./runtime";

export async function getVersion(): Promise<string> {
  if (hasElectron()) {
    return electron().appGetVersion();
  }
  const mod = await import("@tauri-apps/api/app");
  return mod.getVersion();
}

export async function tempDir(): Promise<string> {
  if (hasElectron()) {
    return electron().tempDir();
  }
  const mod = await import("@tauri-apps/api/path");
  return mod.tempDir();
}

export async function relaunch(): Promise<void> {
  if (hasElectron()) {
    await electron().appRelaunch();
    return;
  }
  const mod = await import("@tauri-apps/plugin-process");
  await mod.relaunch();
}

export async function exit(code = 0): Promise<void> {
  if (hasElectron()) {
    await electron().appExit(code);
    return;
  }
  const mod = await import("@tauri-apps/plugin-process");
  await mod.exit(code);
}

// Sync under both runtimes. Tauri's convertFileSrc is sync; Electron's
// preload exposes a sync wrapper too. We eagerly import @tauri-apps/api/core
// here because the same module already underpins invoke/Channel — there's
// no extra cost in the Electron bundle.
import { convertFileSrc as tauriConvertFileSrc } from "@tauri-apps/api/core";

export function convertFileSrc(path: string): string {
  if (hasElectron()) {
    return electron().convertFileSrc(path);
  }
  return tauriConvertFileSrc(path);
}
