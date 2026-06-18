// Auth mutation hooks. Wraps the request_otp / verify_otp / set_pin /
// unlock / initialize_identity flow that all four `(auth)/*` screens
// share. Mirrors the auth shape in `frontend/src/hooks/queries/useAuth.ts`
// where the desktop equivalents live — there's no public hook there, so
// this file is a fresh start scoped to mobile's screen sequence.

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
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
  // Whatever else `crate::commands::pin::UnlockOutcome` carries — opaque
  // to the UI; we read user_id and trust the Rust side to have set
  // AppState.unlock for downstream commands.
}

export interface UnlockStateSnapshot {
  pin_set: boolean;
  is_unlocked: boolean;
  last_active_user: string | null;
}

function profileToUser(p: RawUserProfile): User {
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
      // Stash the recovery key (returned only on first-device signup) so
      // the PIN screen can hand it off to the Emergency Kit display.
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
  const setCurrentUser = appStore.setCurrentUser;
  const logoutStore = appStore.logout;

  return useMutation({
    mutationFn: async (vars: { deleteData?: boolean } | void) => {
      await invoke("logout", { deleteData: vars?.deleteData ?? false });
    },
    onSuccess: () => {
      logoutStore();
      setCurrentUser(null);
    },
  });
}

/**
 * One-shot session-restore call. Used by `app/index.tsx` to decide where
 * to send the user at app start: existing session → tabs, no session →
 * auth flow. Returns the profile (and side-effects the store) when there
 * is one; returns null when not signed in.
 */
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
