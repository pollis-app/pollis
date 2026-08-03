/**
 * Notifications bridge — mirrors `@tauri-apps/plugin-notification`.
 *
 *   - isPermissionGranted() -> boolean
 *   - requestPermission() -> 'granted' | 'denied' | 'default'
 *   - sendNotification({ title, body, icon? })
 *
 * Routed through the plugin's raw IPC names
 * (`plugin:notification|notify`, `plugin:notification|is_permission_granted`,
 * `plugin:notification|request_permission`).
 */

import { invoke } from "./invoke";

export async function isPermissionGranted(): Promise<boolean> {
  const result = await invoke<boolean | null>(
    "plugin:notification|is_permission_granted",
  );
  return result === true;
}

export async function requestPermission(): Promise<
  "granted" | "denied" | "default"
> {
  const state = await invoke<string>(
    "plugin:notification|request_permission",
  );
  if (state === "granted" || state === "denied" || state === "default") {
    return state;
  }
  return "default";
}

export interface NotificationOptions {
  title: string;
  body?: string;
  icon?: string;
}

export async function sendNotification(
  opts: NotificationOptions,
): Promise<void> {
  // Tauri's plugin-notification expects { options: { title, body } } via the
  // raw IPC. Match exactly what utils/notify.ts used to send.
  await invoke("plugin:notification|notify", {
    options: { title: opts.title, body: opts.body },
  });
}
