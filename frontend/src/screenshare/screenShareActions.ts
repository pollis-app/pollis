// Shared screen-share toggle: enumerate → pick → start, plus stop/cancel/recover. Linux's empty list means the portal picks.

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

  // Any other non-idle state (wedged 'starting', 'failed'): force-stop to recover.
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
        // Linux: the stored preference is the only audio choice. Not `starting` yet: a cancelled portal emits no event.
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
