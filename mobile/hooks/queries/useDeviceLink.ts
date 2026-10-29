// QR device links; protocol in docs/qr-device-link-design.md. Mobile never
// shows a v1 code, only scans one (#1207); v2 it both shows and scans (#1361).

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { profileToUser, type RawUserProfile } from "./useAuth";

export async function cancelDeviceLink(linkId: string): Promise<void> {
  await invoke("cancel_device_link", { linkId });
}

/**
 * Sign in by a scanned or pasted QR payload. The profile always requires
 * enrollment: the key arrives when the device that showed the QR approves.
 */
export function useClaimDeviceLink() {
  return useMutation({
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { payload: string; deviceName: string }) =>
      await invoke<RawUserProfile>("claim_device_link", vars),
    onSuccess: (profile) => {
      appStore.setCurrentUser(profileToUser(profile));
      appStore.setUsername(profile.username);
    },
  });
}

export interface DeviceOfferHandle {
  link_id: string;
  /** `pollis-link:v2:<link_id>:<ephemeral public key>` — carries no secret. */
  qr_payload: string;
  /** Unix seconds, server-set. */
  expires_at: number;
}

export type DeviceOfferWait = { state: "signed_in"; profile: RawUserProfile } | { state: "expired" };

export interface AcceptedDeviceOffer {
  link_id: string;
  new_device_id: string;
  device_name: string | null;
}

export type DeviceOfferOutcome =
  | { state: "linked"; device_name: string | null }
  | { state: "tampered" }
  | { state: "expired" };

/** New device: offer a code for a signed-in device to scan. */
export async function createDeviceOffer(deviceName: string): Promise<DeviceOfferHandle> {
  return await invoke<DeviceOfferHandle>("create_device_offer", { deviceName });
}

/**
 * New device: resolves when a signed-in device accepts the code or it lapses.
 * The backoff and the deadline live in Rust.
 */
export async function awaitDeviceOffer(linkId: string): Promise<DeviceOfferWait> {
  const wait = await invoke<DeviceOfferWait>("await_device_offer", { linkId });
  if (wait.state === "signed_in") {
    appStore.setCurrentUser(profileToUser(wait.profile));
    appStore.setUsername(wait.profile.username);
  }
  return wait;
}

export async function cancelDeviceOffer(): Promise<void> {
  await invoke("cancel_device_offer");
}

/** Signed-in device: accept a scanned code. The PIN is verified in Rust first. */
export async function acceptDeviceOffer(
  userId: string,
  pin: string,
  payload: string,
): Promise<AcceptedDeviceOffer> {
  return await invoke<AcceptedDeviceOffer>("accept_device_offer", { userId, pin, payload });
}

/**
 * Signed-in device: wait for the accepted device's enrollment request and wrap
 * the account key to it, only if its key is the one the code showed.
 */
export async function finishDeviceOffer(userId: string, linkId: string): Promise<DeviceOfferOutcome> {
  return await invoke<DeviceOfferOutcome>("finish_device_offer", { userId, linkId });
}
