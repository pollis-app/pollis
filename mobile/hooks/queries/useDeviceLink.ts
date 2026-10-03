// QR device link (#1207). Mirrors pollis-core `commands::device_link`; the
// desktop twin is frontend/src/services/api.ts. Protocol and guardrails:
// docs/qr-device-link-design.md.

import { useMutation } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { profileToUser, type RawUserProfile } from "./useAuth";

export interface DeviceLinkHandle {
  link_id: string;
  qr_payload: string;
  /** Unix seconds, server-set. */
  expires_at: number;
}

export type DeviceLinkStatus =
  | { state: "open" }
  | { state: "claimed"; device_name: string | null }
  | { state: "ready_to_approve"; device_name: string | null; new_device_id: string; request_id: string }
  | { state: "tampered" }
  | { state: "expired" };

/** Create a link to show as a QR. The PIN is verified in Rust. */
export function useCreateDeviceLink() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (pin: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await invoke<DeviceLinkHandle>("create_device_link", { userId: currentUser.id, pin });
    },
  });
}

/**
 * Resolve when the link leaves `since`. One awaited call — backoff and the
 * deadline live in Rust (no polling here).
 */
export async function awaitDeviceLink(
  userId: string,
  linkId: string,
  since: DeviceLinkStatus["state"],
): Promise<DeviceLinkStatus> {
  return await invoke<DeviceLinkStatus>("await_device_link", { userId, linkId, since });
}

export async function approveDeviceLink(userId: string, linkId: string): Promise<void> {
  await invoke("approve_device_link", { userId, linkId });
}

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
    mutationFn: async (vars: { payload: string; deviceName: string }) =>
      await invoke<RawUserProfile>("claim_device_link", vars),
    onSuccess: (profile) => {
      appStore.setCurrentUser(profileToUser(profile));
      appStore.setUsername(profile.username);
    },
  });
}
