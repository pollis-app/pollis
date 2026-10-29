// Auth mutation hooks for the `(auth)/*` screen sequence.

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { endSession } from "../../lib/session";
import { appStore } from "../../stores/appStore";
import type { User } from "../../types";

export interface RawUserProfile {
  id: string;
  email: string;
  username: string;
  new_secret_key?: string;
  enrollment_required?: boolean;
}

export interface UnlockOutcome {
  user_id: string;
  // Whatever else `UnlockOutcome` carries is opaque to the UI.
}

export interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

export function profileToUser(p: RawUserProfile): User {
  const now = Date.now();
  return {
    id: p.id,
    email: p.email,
    username: p.username,
    created_at: now,
    updated_at: now,
  };
}

export function useRequestOtp() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: {} },
    mutationFn: async (email: string) => {
      await invoke("request_otp", { email });
    },
  });
}

export function useVerifyOtp() {
  const setCurrentUser = appStore.setCurrentUser;
  const setUsername = appStore.setUsername;
  const setPendingSecretKey = appStore.setPendingSecretKey;

  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { email: string; code: string }) => {
      const profile = await invoke<RawUserProfile>("verify_otp", {
        email: vars.email,
        code: vars.code,
      });
      return profile;
    },
    onSuccess: (profile) => {
      const user = profileToUser(profile);
      setCurrentUser(user);
      setUsername(profile.username);
      // The recovery key (first-device signup only), for the Emergency Kit.
      if (profile.new_secret_key) {
        setPendingSecretKey(profile.new_secret_key);
      }
    },
  });
}

export function useUnlockState() {
  return useMutation({
    mutationFn: async () => {
      return await invoke<UnlockStateSnapshot>("get_unlock_state");
    },
  });
}

export function useSetPin() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { oldPin?: string; newPin: string }) => {
      await invoke("set_pin", {
        oldPin: vars.oldPin ?? null,
        newPin: vars.newPin,
      });
    },
  });
}

export function useUnlock() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { userId: string; pin: string }) => {
      return await invoke<UnlockOutcome>("unlock", {
        userId: vars.userId,
        pin: vars.pin,
      });
    },
  });
}

export function useInitializeIdentity() {
  return useMutation({
    mutationFn: async (userId: string) => {
      return await invoke<{ user_id: string; public_key: string; is_new: boolean }>(
        "initialize_identity",
        { userId },
      );
    },
  });
}

export function useLogout() {
  return useMutation({
    mutationFn: async (vars: { deleteData?: boolean } | void) => {
      await invoke("logout", { deleteData: vars?.deleteData ?? false });
    },
    // Settled, not just success: a failed `logout` still ends the session.
    onSettled: () => endSession(),
  });
}

/** Session restore for `app/index.tsx`: the profile, or null when not signed in. */
export async function restoreSession(): Promise<RawUserProfile | null> {
  const profile = await invoke<RawUserProfile | null>("get_session");
  if (!profile) {
    return null;
  }
  const { setCurrentUser, setUsername } = appStore;
  setCurrentUser(profileToUser(profile));
  setUsername(profile.username);
  return profile;
}
