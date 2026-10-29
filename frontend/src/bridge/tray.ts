/**
 * Tray bridge (`src-tauri/src/tray.rs`). Linux/Windows: tray always present,
 * unread swaps the icon. macOS: opt-in via `setTrayEnabled`; unread rides the
 * dock badge.
 */

import { invoke, listen } from "./invoke";

/** Mirror the unread count into the tray icon + tooltip (Linux/Windows). */
export async function setTrayUnread(count: number): Promise<void> {
  await invoke("tray_set_unread", { count });
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

/**
 * Subscribe to the tray's "Mute mic" click. Returns an unsubscribe that is safe
 * to call before the async `listen()` resolves.
 */
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
