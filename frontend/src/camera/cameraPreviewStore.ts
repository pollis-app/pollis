import { makeAutoObservable } from "mobx";

import type { CameraSource } from "./types";

/** Settings camera preview (#434), separate from the in-call `CameraState`; off by default. */
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

  /** Keep `preferred` if still present, else the first device; preview stays off. */
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

  /** Change device; a running preview restarts on it. */
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
