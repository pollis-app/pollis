// Shared screen-share toggle action. Drives the enumerate → pick → start
// flow (and the stop / cancel / recover branches) from a single place so
// the green VoiceBar and the in-call voice stage tray behave identically.
//
// macOS and Windows both enumerate and route through the in-app picker
// (macOS via SCShareableContent — the system SCContentSharingPicker has
// an upstream crate bug that crashes on selection, #283; Windows via the
// windows-rs Monitor/Window APIs with GDI thumbnails). Linux's
// xdg-desktop-portal dialog IS the picker, signalled by an empty source
// list from enumerate() — in that case we skip our picker and go
// straight to start().

import { appStore } from "../stores/appStore";
import {
  friendlyScreenShareError,
  screenShareSession,
} from "./screenShareSession";
import type { ShareState } from "../types/voice-state";

export function toggleScreenShare(share: ShareState): void {
  // Already sharing → stop.
  if (share.kind === "active") {
    screenShareSession
      .stop()
      .catch((e) => console.error("[screenshare] stop", e));
    return;
  }

  // Picker open → button doubles as a cancel affordance.
  if (share.kind === "picking") {
    screenShareSession
      .cancelPicker()
      .catch((e) => console.warn("[screenshare] cancel:", e))
      .finally(() => {
        appStore.shareCancelPicker();
      });
    return;
  }

  // Any other non-idle state (e.g. a 'starting' that wedged because
  // publishTrack hung on a dead Wayland-portal track, or 'failed') —
  // let the button recover by force-stopping. shareStopped() is safe
  // from any joined-state.
  if (share.kind !== "idle") {
    screenShareSession
      .stop()
      .catch((e) => console.warn("[screenshare] force-stop:", e));
    return;
  }

  // Engage enumerate → pick → start. The backend returns an empty list on
  // Linux; in that case we skip our picker and go straight to start()
  // (the xdg-desktop-portal dialog handles selection).
  (async () => {
    try {
      const list = await screenShareSession.enumerate();
      if (list.displays.length + list.windows.length === 0) {
        await screenShareSession.start();
        return;
      }
      appStore.shareStartPicking(list);
    } catch (e) {
      console.error("[screenshare] enumerate:", e);
      appStore.shareFailed(friendlyScreenShareError(String(e)));
    }
  })();
}
