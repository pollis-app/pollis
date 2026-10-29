/** Notifications via `@tauri-apps/plugin-notification`'s raw IPC names. */

import { invoke } from "./invoke";
import { escapeNotificationText } from "../utils/notificationText";

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
  await invoke("plugin:notification|notify", {
    options: {
      title: escapeNotificationText(opts.title),
      body: opts.body === undefined ? undefined : escapeNotificationText(opts.body),
    },
  });
}
