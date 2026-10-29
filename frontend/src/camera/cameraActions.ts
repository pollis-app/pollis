// Shared camera toggle, like `toggleScreenShare`; a single camera skips the picker.

import i18n from "../i18n";
import { appStore } from "../stores/appStore";
import { cameraSession, friendlyCameraError } from "./cameraSession";
import type { CameraState } from "../types/voice-state";

export function toggleCamera(camera: CameraState): void {
  if (camera.kind === "active") {
    cameraSession.stop().catch((e) => console.error("[camera] stop", e));
    return;
  }

  if (camera.kind === "picking") {
    appStore.cameraCancelPicker();
    return;
  }

  // Any other non-idle state: force-stop to recover.
  if (camera.kind !== "idle") {
    cameraSession
      .stop()
      .catch((e) => console.warn("[camera] force-stop:", e))
      .finally(() => appStore.cameraStopped());
    return;
  }

  (async () => {
    try {
      const list = await cameraSession.listDevices();
      if (list.cameras.length === 0) {
        appStore.cameraFailed(i18n.t("voice:cameraError.noCamera"));
        return;
      }
      if (list.cameras.length === 1) {
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
