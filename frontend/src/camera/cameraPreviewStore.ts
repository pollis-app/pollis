import { makeAutoObservable } from "mobx";

import type { CameraSource } from "./types";

/**
 * Voice & Video settings camera picker + self-preview state (issue #434).
 *
 * DISTINCT from the in-call `CameraState` (`types/voice-state.ts`), which lives
 * inside a `joined` `VoiceState` because a camera publishes into an active voice
 * room. The settings preview runs locally — no call, nothing published — so it
 * has its own lifecycle here.
 *
 * The union makes invalid states unrepresentable:
 *   - no `deviceId` outside a device-known state → can't preview "nothing";
 *   - an `error` string exists ONLY in `failed` → no stale error beside a live
 *     preview;
 *   - `live`/`starting` can't occur while `loading`/`empty`.
 *
 * Preview is OFF by default (`idle`) — a settings page must not silently light
 * the camera; the user opts in with the toggle (Discord's model).
 */
export type CameraPreviewState =
  | { kind: "loading" }
  | { kind: "empty" }
  | { kind: "idle"; deviceId: string }
  | { kind: "starting"; deviceId: string }
  | { kind: "live"; deviceId: string }
  | { kind: "failed"; deviceId: string; error: string };

class CameraPreviewStore {
  /** Enumerated devices — the dropdown options. Empty until listed. */
  devices: CameraSource[] = [];
  state: CameraPreviewState = { kind: "loading" };

  constructor() {
    makeAutoObservable(this);
  }

  /** The device selected in the picker, or `null` before enumeration. */
  get selectedDeviceId(): string | null {
    return "deviceId" in this.state ? this.state.deviceId : null;
  }

  /** True while a live preview is running or spinning up. */
  get isPreviewing(): boolean {
    return this.state.kind === "live" || this.state.kind === "starting";
  }

  /** Enumeration finished. `preferred` is the persisted choice — kept if still
   *  present, else the first device. Preview stays OFF. */
  enumerated(devices: CameraSource[], preferred: string | null) {
    this.devices = devices;
    if (devices.length === 0) {
      this.state = { kind: "empty" };
      return;
    }
    const deviceId = devices.some((d) => d.id === preferred) ? preferred! : devices[0].id;
    this.state = { kind: "idle", deviceId };
  }

  /** Enumeration failed — treat as "no camera" (nothing to preview). */
  enumerationFailed() {
    this.devices = [];
    this.state = { kind: "empty" };
  }

  /** Pick a different device. If a preview is running it stays running (moves to
   *  `starting` — the caller restarts capture on the new device); if it's off it
   *  just changes the selection. */
  select(deviceId: string) {
    if (!this.devices.some((d) => d.id === deviceId)) {
      return;
    }
    switch (this.state.kind) {
      case "idle":
      case "failed":
        this.state = { kind: "idle", deviceId };
        break;
      case "starting":
      case "live":
        this.state = { kind: "starting", deviceId };
        break;
      // loading / empty: nothing selectable.
    }
  }

  /** Preview turn-on requested for the current device. */
  startRequested() {
    const id = this.selectedDeviceId;
    if (id !== null) {
      this.state = { kind: "starting", deviceId: id };
    }
  }

  /** Capture confirmed live (the `start_camera_preview` invoke resolved). */
  wentLive() {
    if (this.state.kind === "starting") {
      this.state = { kind: "live", deviceId: this.state.deviceId };
    }
  }

  /** Preview failed (capture error / permission denied). */
  failed(error: string) {
    const id = this.selectedDeviceId;
    if (id !== null) {
      this.state = { kind: "failed", deviceId: id, error };
    }
  }

  /** Preview turned off — back to `idle` on the same device. */
  stopped() {
    const id = this.selectedDeviceId;
    this.state = id !== null ? { kind: "idle", deviceId: id } : { kind: "loading" };
  }

  /** Full reset on leaving the page. */
  reset() {
    this.devices = [];
    this.state = { kind: "loading" };
  }
}

export const cameraPreviewStore = new CameraPreviewStore();
