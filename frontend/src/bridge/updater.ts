/**
 * Updater bridge — wraps `@tauri-apps/plugin-updater` (Tauri) and
 * `electron-updater` (Electron) behind a single `check()` returning a
 * `PollisUpdate` with the shape the existing UpdateScreen + Settings
 * auto-update flows already speak.
 *
 * Under Electron in dev (`!app.isPackaged`), `check()` returns `null` —
 * autoUpdater requires a packaged + signed build to do anything real, and
 * we don't want the dev UI to throw on its mount check.
 */

import { hasElectron } from "./runtime";

export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | {
      event: "Progress";
      data: {
        chunkLength: number;
        // Electron path forwards electron-updater's precomputed
        // `percent` (0–100, float) directly when available. Renderers
        // should prefer this over summing chunkLength because the
        // `Started` event does NOT carry the file size on Electron
        // (electron-updater learns it only when bytes start flowing),
        // so the chunkLength-sum / contentLength compute can't run.
        // Tauri's plugin still ships only chunkLength + an upfront
        // contentLength — both code paths are handled below.
        percent?: number;
        transferred?: number;
        total?: number;
      };
    }
  | { event: "Finished"; data: Record<string, never> };

export interface PollisUpdate {
  version: string;
  downloadAndInstall(progress?: (e: DownloadEvent) => void): Promise<void>;
}

interface ElectronUpdaterAPI {
  updaterCheck: () => Promise<{ version: string } | null>;
  updaterDownloadAndInstall: () => Promise<void>;
  updaterOnEvent: (cb: (envelope: DownloadEvent) => void) => () => void;
}

function electronAPI(): ElectronUpdaterAPI {
  const w = window as unknown as { electronAPI?: ElectronUpdaterAPI };
  if (!w.electronAPI) {
    throw new Error("electronAPI not exposed");
  }
  return w.electronAPI;
}

export async function check(): Promise<PollisUpdate | null> {
  if (hasElectron()) {
    const api = electronAPI();
    const info = await api.updaterCheck();
    if (!info) {
      return null;
    }
    return {
      version: info.version,
      async downloadAndInstall(progress) {
        const unlisten = progress
          ? api.updaterOnEvent((e) => progress(e))
          : null;
        try {
          // Main process kicks off the download; the 'update-downloaded'
          // handler calls quitAndInstall when bytes finish — so this
          // promise resolves at the start of install, not the end. Matches
          // Tauri's downloadAndInstall semantics: caller flips UI to
          // "installing" / "relaunching" after this resolves and the OS
          // takes over via app relaunch.
          await api.updaterDownloadAndInstall();
        } finally {
          unlisten?.();
        }
      },
    };
  }
  const mod = await import("@tauri-apps/plugin-updater");
  const update = await mod.check();
  if (!update) {
    return null;
  }
  return update as unknown as PollisUpdate;
}
