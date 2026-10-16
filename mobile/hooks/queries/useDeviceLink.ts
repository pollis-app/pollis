// QR device links. Mirrors pollis-core `commands::device_link`; the desktop
// twin is frontend/src/services/api.ts. Protocol and guardrails:
// docs/qr-device-link-design.md.
//
// Mobile plays three of the four roles: the new phone scans a code a desktop
// shows (v1, #1207, `useClaimDeviceLink`) or shows its own (v2, #1361), and a
// signed-in phone scans a new device's code (v2). It no longer SHOWS a v1 code
// for others to scan: that is the scanner's job now.

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { profileToUser, type RawUserProfile } from "./useAuth";

/** Forget a link this device was following (the scanner's accepted code). */
export async function cancelDeviceLink(linkId: string): Promise<void> {
  await invoke("cancel_device_link", { linkId });
}

/**
 * Sign in by a scanned or pasted QR payload. Sets the signed-in user exactly
 * as `useVerifyOtp` does; the profile always requires enrollment (the key is
 * handed over when the device that showed the QR approves).
 */
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

// ── v2 (#1361): the NEW device shows the code, a signed-in device scans it ──

export interface DeviceOfferHandle {
  link_id: string;
  /** `pollis-link:v2:<link_id>:<ephemeral public key>` — carries no secret. */
  qr_payload: string;
  /** Unix seconds, server-set. Show a fresh code after this. */
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
 * New device: resolve when a signed-in device accepts the code (this device is
 * then signed in and must enroll) or when it lapses. One awaited call — the
 * backoff and the deadline live in Rust. Sets the signed-in user exactly as
 * `useVerifyOtp` does.
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
 * the account key to it — only if its key is the one the code showed. One
 * awaited call.
 */
export async function finishDeviceOffer(userId: string, linkId: string): Promise<DeviceOfferOutcome> {
  return await invoke<DeviceOfferOutcome>("finish_device_offer", { userId, linkId });
}
