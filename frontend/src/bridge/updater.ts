/** `check()` over `@tauri-apps/plugin-updater`, using Rust's `get_update_check_plan` so the overlay proxy covers manifest and download. */

import { invoke } from "./invoke";

// Mirrors the plugin's DownloadEvent.
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
  // No command or an error: direct, as before the overlay.
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
