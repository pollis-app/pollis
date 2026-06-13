/**
 * Tray bridge — the system-tray / menu-bar surface, dual-runtime.
 *
 * Under Electron, routes to the `electronAPI.tray*` methods exposed by the
 * preload (backed by `electron/src/tray.ts`). Under Tauri, routes to the
 * `tray_set_*` commands + the `tray:requestToggleMute` event emitted by
 * `src-tauri/src/tray.rs`. Both backends share identical semantics:
 *
 *   - Linux/Windows: tray always present; unread swaps the icon; close
 *     hides to tray when `setTrayCloseToTray(true)`.
 *   - macOS: tray opt-in via `setTrayEnabled`; unread rides the dock badge.
 */

import { electron, hasElectron } from "./runtime";
import { invoke, listen } from "./invoke";

/** Mirror the unread count into the tray icon + tooltip (Linux/Windows). */
export async function setTrayUnread(count: number): Promise<void> {
  if (hasElectron()) {
    await electron().traySetUnread(count);
    return;
  }
  await invoke("tray_set_unread", { count });
}

/** Toggle hide-to-tray-on-close (Linux/Windows). No-op on macOS. */
export async function setTrayCloseToTray(enabled: boolean): Promise<void> {
  if (hasElectron()) {
    await electron().traySetCloseToTray(enabled);
    return;
  }
  await invoke("tray_set_close_to_tray", { enabled });
}

/** Enable/disable the menu-bar tray icon (macOS only). */
export async function setTrayEnabled(enabled: boolean): Promise<void> {
  if (hasElectron()) {
    await electron().traySetEnabled(enabled);
    return;
  }
  await invoke("tray_set_enabled", { enabled });
}

/** Push live call + mute state so the tray "Mute mic" item stays accurate. */
export async function setTrayVoiceState(
  inCall: boolean,
  muted: boolean,
): Promise<void> {
  if (hasElectron()) {
    await electron().traySetVoiceState(inCall, muted);
    return;
  }
  await invoke("tray_set_voice_state", { inCall, muted });
}

/**
 * Subscribe to "user clicked Mute mic in the tray menu". Returns an
 * unsubscribe fn. Electron is synchronous; Tauri resolves the listener
 * asynchronously, so we guard against an unsubscribe that races the
 * pending `listen()`.
 */
export function onTrayRequestToggleMute(cb: () => void): () => void {
  if (hasElectron()) {
    return electron().trayOnRequestToggleMute(cb);
  }
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
