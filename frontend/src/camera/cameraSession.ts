// Webcam session glue: subscribes to the backend camera-event Channel once per
// process and mirrors the lifecycle into the store. Frames (local preview and
// remote) ride screen share's loopback frame WebSocket, keyed by trackKey, so a
// camera tile subscribes via `screenShareSession.onFrame(trackKey)`.

import { reaction } from "mobx";

import { Channel, invoke } from "../bridge";
import i18n from "../i18n";
import { appStore } from "../stores/appStore";
import type { CameraEvent, CameraList } from "./types";

/** Must match `LOCAL_CAMERA_PREVIEW_KEY` in `pollis-core/src/commands/camera/mod.rs`;
 *  distinct from screen share's `LOCAL_PREVIEW_KEY` so both previews can run. */
export const LOCAL_CAMERA_PREVIEW_KEY = "__local_camera_preview__";

/** Collapse a raw backend camera error into one status-bar sentence; unknown
 *  shapes pass through. Uses `i18n.t`: the string is stored once, not re-rendered. */
export function friendlyCameraError(raw: string): string {
  const r = raw.toLowerCase();
  if (
    r.includes("not yet supported") ||
    r.includes("unsupported")
  ) {
    return i18n.t("voice:cameraError.unsupported");
  }
  if (
    r.includes("permission") ||
    r.includes("denied") ||
    r.includes("not authorized") ||
    r.includes("tcc")
  ) {
    return i18n.t("voice:cameraError.permission");
  }
  if (r.includes("busy") || r.includes("in use") || r.includes("ebusy")) {
    return i18n.t("voice:cameraError.busy");
  }
  if (
    r.includes("helper binary") ||
    r.includes("helper not found") ||
    r.includes("no such file")
  ) {
    return i18n.t("voice:cameraError.helperMissing");
  }
  if (r.includes("no camera") || r.includes("no devices") || r.includes("no video")) {
    return i18n.t("voice:cameraError.noCamera");
  }
  return raw;
}

class CameraSession {
  private subscribed = false;
  // Detached on logout so a late event can't mutate the just-reset store.
  private eventsChannel: Channel<CameraEvent> | null = null;

  constructor() {
    reaction(
      () => appStore.currentUser,
      (user) => {
        if (!user) {
          this.teardown();
        }
      },
    );
  }

  teardown(): void {
    this.subscribed = false;
    if (this.eventsChannel) {
      this.eventsChannel.onmessage = () => {};
      this.eventsChannel = null;
    }
  }

  async ensureSubscribed(): Promise<void> {
    if (this.subscribed) {
      return;
    }
    this.subscribed = true;
    const events = new Channel<CameraEvent>();
    events.onmessage = (ev) => this.handleEvent(ev);
    this.eventsChannel = events;
    await invoke("subscribe_camera_events", { onEvent: events });
  }

  /** The backend parks the helper waiting for `start(deviceId)`, so pair an
   *  enumerate that leads to a pick with a `start` or a `stop`. */
  async listDevices(): Promise<CameraList> {
    return await invoke<CameraList>("list_video_devices");
  }

  async start(deviceId: string): Promise<void> {
    await invoke("start_camera", { deviceId });
  }

  async stop(): Promise<void> {
    await invoke("stop_camera");
  }

  /** Preview-only capture for the settings picker (#434): no voice room, nothing
   *  published, independent of an in-call camera. Pair with {@link stopPreview}. */
  async startPreview(deviceId: string): Promise<void> {
    await invoke("start_camera_preview", { deviceId });
  }

  async stopPreview(): Promise<void> {
    await invoke("stop_camera_preview");
  }

  private handleEvent(ev: CameraEvent) {
    const store = appStore;
    switch (ev.type) {
      case "local_started":
        // Same starting → active path whoever initiated (picker vs recovery).
        store.cameraStartStarting();
        store.cameraStarted("camera", { width: ev.width, height: ev.height });
        break;
      case "local_stopped":
        store.cameraStopped();
        break;
      case "local_error":
        store.cameraFailed(friendlyCameraError(ev.message));
        break;
    }
  }
}

export const cameraSession = new CameraSession();
