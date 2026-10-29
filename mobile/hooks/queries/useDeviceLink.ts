// QR device links (pollis-core `commands::device_link`; docs/qr-device-link-design.md).

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { profileToUser, type RawUserProfile } from "./useAuth";

export async function cancelDeviceLink(linkId: string): Promise<void> {
  await invoke("cancel_device_link", { linkId });
}

/** Sign in by a scanned or pasted QR payload; always requires enrollment. */
export function useClaimDeviceLink() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { payload: string; deviceName: string }) =>
      await invoke<RawUserProfile>("claim_device_link", vars),
    onSuccess: (profile) => {
      appStore.setCurrentUser(profileToUser(profile));
      appStore.setUsername(profile.username);
    },
  });
}

// v2 (#1361): the NEW device shows the code, a signed-in device scans it.

export interface DeviceOfferHandle {
  link_id: string;
  /** Carries no secret. */
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

export async function createDeviceOffer(deviceName: string): Promise<DeviceOfferHandle> {
  return await invoke<DeviceOfferHandle>("create_device_offer", { deviceName });
}

/** New device: one awaited call until accepted or lapsed; backoff lives in Rust. */
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

/** The PIN is verified in Rust first. */
export async function acceptDeviceOffer(
  userId: string,
  pin: string,
  payload: string,
): Promise<AcceptedDeviceOffer> {
  return await invoke<AcceptedDeviceOffer>("accept_device_offer", { userId, pin, payload });
}

/** Wrap the account key to the accepted device, only if its key is the one the code showed. */
export async function finishDeviceOffer(userId: string, linkId: string): Promise<DeviceOfferOutcome> {
  return await invoke<DeviceOfferOutcome>("finish_device_offer", { userId, linkId });
}
