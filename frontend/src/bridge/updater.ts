/**
 * Updater bridge — wraps `@tauri-apps/plugin-updater`.
 *
 * Under Tauri: delegates to the real plugin so existing UpdateScreen +
 * Settings auto-update + manual-check flows keep working.
 *
 * Under Electron: stubbed pending Phase 7 (electron-updater integration).
 * `check()` throws so callers fall through to their error path instead of
 * silently succeeding.
 */

import { hasElectron } from "./runtime";

export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished"; data: Record<string, never> };

export interface PollisUpdate {
  version: string;
  downloadAndInstall(progress?: (e: DownloadEvent) => void): Promise<void>;
}

export async function check(): Promise<PollisUpdate | null> {
  if (hasElectron()) {
    throw new Error("Phase 7: electron-updater not yet wired");
  }
  const mod = await import("@tauri-apps/plugin-updater");
  const update = await mod.check();
  if (!update) {
    return null;
  }
  return update as unknown as PollisUpdate;
}
