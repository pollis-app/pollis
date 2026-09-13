/**
 * Updater bridge — wraps `@tauri-apps/plugin-updater` behind a single
 * `check()` returning a `PollisUpdate` with the shape the existing
 * UpdateScreen + Settings auto-update flows already speak.
 *
 * ## The overlay
 *
 * The updater is the one first-party HTTP caller that does not go through
 * `pollis_relay::http::http_client`: the plugin builds its own `reqwest`
 * client inside Rust. So with the relay overlay on, every other request rode
 * the relay and this one still went straight to `cdn.pollis.com`, handing it
 * the device's real address on every window focus — including in `strict`,
 * the mode whose entire promise is that nothing silently goes direct.
 *
 * Rust decides what is allowed (`get_update_check_plan`, see
 * `pollis-core/src/commands/update.rs`) and this passes the answer to the
 * plugin. `check` stores the proxy on the `Update` it returns, so the artifact
 * download inherits it and not just the manifest fetch.
 */

import { invoke } from "./invoke";

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

/** Mirrors `pollis_core::commands::update::UpdateCheckPlan`. */
export type UpdateCheckPlan =
  | { kind: "direct" }
  | { kind: "proxy"; url: string }
  | { kind: "blocked"; reason: string };

export async function check(): Promise<PollisUpdate | null> {
  // Ask Rust first. A build without the command (or an error reading the
  // overlay) falls back to a direct check, which is what every pre-overlay
  // build did — the overlay is off by default and off means byte-for-byte the
  // old behaviour.
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
