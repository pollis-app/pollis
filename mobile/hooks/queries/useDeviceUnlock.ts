// Biometric unlock state for Security (#1685): whether core has confirmed slots, and turning it off.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { disableDeviceUnlock } from "../../lib/deviceUnlock";
import { appStore } from "../../stores/appStore";
import type { UnlockStateSnapshot } from "./useAuth";

export const DEVICE_UNLOCK_QUERY_KEY = ["deviceUnlock"] as const;

export function useDeviceUnlock() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: [...DEVICE_UNLOCK_QUERY_KEY, userId],
    queryFn: async (): Promise<boolean> =>
      (await invoke<UnlockStateSnapshot>("get_unlock_state")).device_unlock === true,
    enabled: !!userId,
  });

  // Turning it ON asks for the PIN first (app/self/biometric-unlock.tsx); off needs nothing.
  const disable = useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "toggle" } },
    mutationFn: async () => {
      if (!userId) {
        return;
      }
      await disableDeviceUnlock(userId);
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: DEVICE_UNLOCK_QUERY_KEY }),
  });

  return {
    enabled: query.data === true,
    isLoading: query.isPending,
    disable,
  };
}
