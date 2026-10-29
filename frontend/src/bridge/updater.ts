/**
 * Updater bridge — `check()` returning a `PollisUpdate`.
 *
 * The plugin builds its own `reqwest` client, bypassing the relay overlay, so
 * Rust decides the route (`get_update_check_plan`) and `check` stores the proxy
 * on the `Update` so the download inherits it too, not just the manifest.
 */

import { invoke } from "./invoke";

// Mirrors the plugin's DownloadEvent: `Progress` carries per-chunk bytes only.
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

/** Mirrors `pollis_core::commands::update::UpdateCheckPlan`. */
export type UpdateCheckPlan =
  | { kind: "direct" }
  | { kind: "proxy"; url: string }
  | { kind: "blocked"; reason: string };

export async function check(): Promise<PollisUpdate | null> {
  // A build without the command, or an error reading the overlay, checks
  // direct, which is what overlay-off means.
  let plan: UpdateCheckPlan = { kind: "direct" };
  try {
    const answer = await invoke<UpdateCheckPlan>("get_update_check_plan");
    if (answer && typeof answer.kind === "string") {
      plan = answer;
    }
  } catch (err) {
    console.warn("[updater] could not read the overlay plan:", err);
  }
  if (plan.kind === "blocked") {
    console.info("[updater] update check skipped:", plan.reason);
    return null;
  }

  const mod = await import("@tauri-apps/plugin-updater");
  const update = await mod.check(
    plan.kind === "proxy" ? { proxy: plan.url } : undefined,
  );
  if (!update) {
    return null;
  }
  return update as unknown as PollisUpdate;
}
