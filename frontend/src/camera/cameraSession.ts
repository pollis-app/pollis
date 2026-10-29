// Webcam session glue; frames ride `screenShareSession`'s loopback socket. Capture is Rust-side everywhere.

import { reaction } from "mobx";

import { Channel, invoke } from "../bridge";
import i18n from "../i18n";
import { appStore } from "../stores/appStore";
import type { CameraEvent, CameraList } from "./types";

/** Must match `LOCAL_CAMERA_PREVIEW_KEY` in `pollis-core/src/commands/camera/mod.rs`. */
export const LOCAL_CAMERA_PREVIEW_KEY = "__local_camera_preview__";

/** Collapse known camera errors into one sentence; others pass through. Resolved per call via `i18n.t`. */
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
  // Held so logout can detach it.
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

  /** Idempotent. Wire the backend camera-event Channel once after auth. */
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

  /** Enumerate devices; pair it with `start` or `stop` (the helper is parked). */
  async listDevices(): Promise<CameraList> {
    return await invoke<CameraList>("list_video_devices");
  }

  /** Start capture and publish into the voice room. */
  async start(deviceId: string): Promise<void> {
    await invoke("start_camera", { deviceId });
  }

  async stop(): Promise<void> {
    await invoke("stop_camera");
  }

  /** Preview-only capture for the settings picker (#434); nothing published. */
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
