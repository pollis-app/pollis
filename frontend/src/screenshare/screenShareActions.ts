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

  // The source list (or the portal dialog) is still being awaited: a second click is the same intent.
  if (share.kind === "enumerating") {
    return;
  }

  // Any other non-idle state (wedged 'starting', 'failed'): force-stop to recover. A stop with
  // nothing live sends no `local_stopped`, so the store is reset here.
  if (share.kind !== "idle") {
    screenShareSession
      .stop()
      .catch((e) => console.warn("[screenshare] force-stop:", e))
      .finally(() => appStore.shareStopped());
    return;
  }

  // Busy on the same frame as the click, before anything is awaited.
  appStore.shareStartEnumerating();
  (async () => {
    try {
      const list = await screenShareSession.enumerate();
      if (list.displays.length + list.windows.length === 0) {
        // Linux: the stored preference is the only audio choice. A cancelled portal resolves with no event.
        const withAudio = await screenShareSession.resolveWithAudio();
        await screenShareSession.start(undefined, withAudio);
        appStore.shareEnumerationSettled();
        return;
      }
      appStore.shareStartPicking(list);
    } catch (e) {
      console.error("[screenshare] enumerate:", e);
      appStore.shareFailed(friendlyScreenShareError(String(e)));
    }
  })();
}
