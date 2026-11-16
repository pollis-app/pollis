/** Tray / menu-bar bridge over `src-tauri/src/tray.rs`; macOS is opt-in. */

import { invoke, listen } from "./invoke";

/** Mirror the unread count into the tray icon + tooltip (Linux/Windows); `tooltip` is the translated text. */
export async function setTrayUnread(count: number, tooltip?: string): Promise<void> {
  await invoke("tray_set_unread", { count, tooltip });
}

/** The tray menu's copy in the user's language (#1724); the shell has no catalogues of its own. */
export interface TrayLabels {
  open: string;
  muteMic: string;
  unmuteMic: string;
  muteMicIdle: string;
  version: string;
  quit: string;
}

export async function setTrayLabels(labels: TrayLabels): Promise<void> {
  await invoke("tray_set_labels", { labels });
}

/** Toggle hide-to-tray-on-close (Linux/Windows). No-op on macOS. */
export async function setTrayCloseToTray(enabled: boolean): Promise<void> {
  await invoke("tray_set_close_to_tray", { enabled });
}

/** Enable/disable the menu-bar tray icon (macOS only). */
export async function setTrayEnabled(enabled: boolean): Promise<void> {
  await invoke("tray_set_enabled", { enabled });
}

/** Push live call + mute state so the tray "Mute mic" item stays accurate. */
export async function setTrayVoiceState(
  inCall: boolean,
  muted: boolean,
): Promise<void> {
  await invoke("tray_set_voice_state", { inCall, muted });
}

/** Subscribe to the tray's "Mute mic"; guards an unsubscribe racing `listen()`. */
export function onTrayRequestToggleMute(cb: () => void): () => void {
  let unlisten: (() => void) | null = null;
  let cancelled = false;
  void listen("tray:requestToggleMute", () => cb()).then((u) => {
    if (cancelled) {
      u();
    } else {
      unlisten = u;
    }
  });
  return () => {
    cancelled = true;
    if (unlisten) {
      unlisten();
    }
  };
}
