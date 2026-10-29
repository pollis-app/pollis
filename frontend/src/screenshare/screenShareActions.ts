// Screen-share toggle shared by the VoiceBar and the stage tray. macOS and
// Windows use the in-app picker (macOS's SCContentSharingPicker crashes on
// selection, #283); on Linux the portal dialog is the picker, signalled by an
// empty source list, so start() is called directly.

import { appStore } from "../stores/appStore";
import {
  friendlyScreenShareError,
  screenShareSession,
} from "./screenShareSession";
import type { ShareState } from "../types/voice-state";

export function toggleScreenShare(share: ShareState): void {
  if (share.kind === "active") {
    screenShareSession
      .stop()
      .catch((e) => console.error("[screenshare] stop", e));
    return;
  }

  if (share.kind === "picking") {
    screenShareSession
      .cancelPicker()
      .catch((e) => console.warn("[screenshare] cancel:", e))
      .finally(() => {
        appStore.shareCancelPicker();
      });
    return;
  }

  // Any other non-idle state (a wedged 'starting', 'failed'): force-stop to recover.
  if (share.kind !== "idle") {
    screenShareSession
      .stop()
      .catch((e) => console.warn("[screenshare] force-stop:", e));
    return;
  }

  (async () => {
    try {
      const list = await screenShareSession.enumerate();
      if (list.displays.length + list.windows.length === 0) {
        // Linux: no in-app step, so audio comes from the stored preference.
        // Not moved to `starting` first: cancelling the portal returns a plain
        // `Ok` with no event, which would leave the pill wedged; the state
        // advances on `local_started`.
        const withAudio = await screenShareSession.resolveWithAudio();
        await screenShareSession.start(undefined, withAudio);
        return;
      }
      appStore.shareStartPicking(list);
    } catch (e) {
      console.error("[screenshare] enumerate:", e);
      appStore.shareFailed(friendlyScreenShareError(String(e)));
    }
  })();
}
