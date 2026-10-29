import { makeAutoObservable } from "mobx";

import type { CameraSource } from "./types";

/**
 * Settings-page camera picker + self-preview (#434). Distinct from the in-call
 * `CameraState`: the preview publishes nothing, so it has its own lifecycle.
 * Off by default (`idle`) so a settings page never silently lights the camera.
 */
export type CameraPreviewState =
  | { kind: "loading" }
  | { kind: "empty" }
  | { kind: "idle"; deviceId: string }
  | { kind: "starting"; deviceId: string }
  | { kind: "live"; deviceId: string }
  | { kind: "failed"; deviceId: string; error: string };

class CameraPreviewStore {
  devices: CameraSource[] = [];
  state: CameraPreviewState = { kind: "loading" };

  constructor() {
    makeAutoObservable(this);
  }

  get selectedDeviceId(): string | null {
    return "deviceId" in this.state ? this.state.deviceId : null;
  }

  get isPreviewing(): boolean {
    return this.state.kind === "live" || this.state.kind === "starting";
  }

  /** `preferred` is kept if still present, else the first device. Preview stays OFF. */
  enumerated(devices: CameraSource[], preferred: string | null) {
    this.devices = devices;
    if (devices.length === 0) {
      this.state = { kind: "empty" };
      return;
    }
    const deviceId = devices.some((d) => d.id === preferred) ? preferred! : devices[0].id;
    this.state = { kind: "idle", deviceId };
  }

  enumerationFailed() {
    this.devices = [];
    this.state = { kind: "empty" };
  }

  /** A running preview stays running (`starting`; the caller restarts capture). */
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

  startRequested() {
    const id = this.selectedDeviceId;
    if (id !== null) {
      this.state = { kind: "starting", deviceId: id };
    }
  }

  wentLive() {
    if (this.state.kind === "starting") {
      this.state = { kind: "live", deviceId: this.state.deviceId };
    }
  }

  failed(error: string) {
    const id = this.selectedDeviceId;
    if (id !== null) {
      this.state = { kind: "failed", deviceId: id, error };
    }
  }

  stopped() {
    const id = this.selectedDeviceId;
    this.state = id !== null ? { kind: "idle", deviceId: id } : { kind: "loading" };
  }

  reset() {
    this.devices = [];
    this.state = { kind: "loading" };
  }
}

export const cameraPreviewStore = new CameraPreviewStore();
