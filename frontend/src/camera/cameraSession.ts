// Webcam session glue. The camera counterpart of `screenShareSession`:
// subscribes to the backend camera-event Channel once per process and
// mirrors the lifecycle into the MobX store.
//
// Frame transport is deliberately NOT reimplemented here. Local self-preview
// and remote camera frames both ride the SAME loopback frame WebSocket the
// screen-share path already owns (the Rust side mirrors local webcam frames
// under `LOCAL_CAMERA_PREVIEW_KEY` and remote camera tracks flow through the
// shared remote-video drain). So a camera tile just subscribes via
// `screenShareSession.onFrame(trackKey)` like any other video tile — there's
// one socket, one decoder, keyed by trackKey.
//
// Camera capture is Rust-side on every platform (macOS AVFoundation, Linux
// V4L2, Windows Media Foundation), so — unlike screen share — there is no
// Electron renderer branch: the deprecated Electron shell never grew a JS
// camera path, and Tauri is the shipping shell.

import { reaction } from "mobx";

import { Channel, invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import type { CameraEvent, CameraList } from "./types";

/** Reserved frame-WS track key the backend mirrors the local outgoing
 *  webcam under, for the sharer's own preview. Must match
 *  `LOCAL_CAMERA_PREVIEW_KEY` in `pollis-core/src/commands/camera/mod.rs`.
 *  Distinct from screen share's `LOCAL_PREVIEW_KEY` so running both at once
 *  doesn't cross the two previews. */
export const LOCAL_CAMERA_PREVIEW_KEY = "__local_camera_preview__";

/** Collapse a raw backend camera error into one clear status-bar sentence.
 *  Unknown shapes pass through unchanged so a novel error is never hidden. */
export function friendlyCameraError(raw: string): string {
  const r = raw.toLowerCase();
  if (
    r.includes("not yet supported") ||
    r.includes("unsupported")
  ) {
    return "Webcam capture isn't available on this platform yet.";
  }
  if (
    r.includes("permission") ||
    r.includes("denied") ||
    r.includes("not authorized") ||
    r.includes("tcc")
  ) {
    return "Camera access is blocked. Grant Pollis camera permission, then try again.";
  }
  if (r.includes("busy") || r.includes("in use") || r.includes("ebusy")) {
    return "The camera is in use by another app. Close it and try again.";
  }
  if (
    r.includes("helper binary") ||
    r.includes("helper not found") ||
    r.includes("no such file")
  ) {
    return "Camera helper is missing. Reinstall Pollis to restore it.";
  }
  if (r.includes("no camera") || r.includes("no devices") || r.includes("no video")) {
    return "No webcam was found.";
  }
  return raw;
}

class CameraSession {
  private subscribed = false;
  // Held so teardown can detach the handler on logout — a late event would
  // otherwise mutate the just-reset store.
  private eventsChannel: Channel<CameraEvent> | null = null;

  constructor() {
    // Mirror screenShareSession: drop the event subscription on logout.
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

  /** Enumerate capture devices. The backend spawns the helper and parks it
   *  waiting for the upcoming `start(deviceId)`, so always pair an
   *  enumerate that leads to a pick with a `start` or a `stop`. */
  async listDevices(): Promise<CameraList> {
    return await invoke<CameraList>("list_video_devices");
  }

  /** Start capture with the picked device. The backend publishes a
   *  `TrackSource::Camera` track into the active voice room and begins
   *  mirroring local frames under `LOCAL_CAMERA_PREVIEW_KEY`. */
  async start(deviceId: string): Promise<void> {
    await invoke("start_camera", { deviceId });
  }

  async stop(): Promise<void> {
    await invoke("stop_camera");
  }

  /** Start a PREVIEW-ONLY capture for the settings picker (issue #434): local
   *  self-preview under `LOCAL_CAMERA_PREVIEW_KEY` with no voice room and
   *  nothing published. Independent of {@link start} — safe to run out of a
   *  call, and does not disturb an in-call camera. Pair with {@link stopPreview}
   *  when the picker closes or the device changes. */
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
        // The backend signals start after its helper has published. Drive
        // the store through starting → active so the union lands in the
        // same shape regardless of who initiated (picker vs recovery).
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
