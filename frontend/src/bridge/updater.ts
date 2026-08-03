/**
 * Updater bridge — wraps `@tauri-apps/plugin-updater` behind a single
 * `check()` returning a `PollisUpdate` with the shape the existing
 * UpdateScreen + Settings auto-update flows already speak.
 */

// Mirrors `@tauri-apps/plugin-updater`'s DownloadEvent: `Started` carries the
// upfront content length, `Progress` carries per-chunk byte counts only (no
// precomputed percentage), `Finished` carries nothing.
export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | {
      event: "Progress";
      data: {
        chunkLength: number;
      };
    }
  | { event: "Finished"; data: Record<string, never> };

export interface PollisUpdate {
  version: string;
  downloadAndInstall(progress?: (e: DownloadEvent) => void): Promise<void>;
}

export async function check(): Promise<PollisUpdate | null> {
  const mod = await import("@tauri-apps/plugin-updater");
  const update = await mod.check();
  if (!update) {
    return null;
  }
  return update as unknown as PollisUpdate;
}
