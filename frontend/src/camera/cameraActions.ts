// Shared camera toggle action — the camera counterpart of
// `toggleScreenShare`. Drives enumerate → (pick) → start and the
// stop / cancel / recover branches from one place so the VoiceBar pill and
// the in-call stage tray behave identically.
//
// Unlike screen share, every platform enumerates a real device list, so the
// in-app picker is the norm. With exactly one camera we skip the picker and
// start straight away (Discord/Zoom do the same) — the picker only earns its
// keep when there's a choice to make.

import { appStore } from "../stores/appStore";
import { cameraSession, friendlyCameraError } from "./cameraSession";
import type { CameraState } from "../types/voice-state";

export function toggleCamera(camera: CameraState): void {
  // Already on → turn off.
  if (camera.kind === "active") {
    cameraSession.stop().catch((e) => console.error("[camera] stop", e));
    return;
  }

  // Picker open → the button doubles as cancel.
  if (camera.kind === "picking") {
    appStore.cameraCancelPicker();
    return;
  }

  // Any other non-idle state (a wedged 'starting', or 'failed') — recover by
  // force-stopping. cameraStopped() is safe from any joined-state.
  if (camera.kind !== "idle") {
    cameraSession
      .stop()
      .catch((e) => console.warn("[camera] force-stop:", e))
      .finally(() => appStore.cameraStopped());
    return;
  }

  // Engage enumerate → (pick) → start.
  (async () => {
    try {
      const list = await cameraSession.listDevices();
      if (list.cameras.length === 0) {
        appStore.cameraFailed("No webcam was found.");
        return;
      }
      if (list.cameras.length === 1) {
        // One camera — no decision to make, start it directly.
        appStore.cameraStartStarting();
        await cameraSession.start(list.cameras[0].id);
        return;
      }
      appStore.cameraStartPicking(list.cameras);
    } catch (e) {
      console.error("[camera] enumerate:", e);
      appStore.cameraFailed(friendlyCameraError(String(e)));
    }
  })();
}
