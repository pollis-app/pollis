// Device list / revoke for the Security screen; Rust refuses revoking the current device.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { securityEventQueryKeys } from "./useSecurityEvents";

export interface DeviceRow {
  device_id: string;
  device_name: string | null;
  created_at: string;
  last_seen: string;
  is_current: boolean;
}

export const deviceQueryKeys = {
  list: (userId: string | null) => ["devices", userId] as const,
};

export function useUserDevices() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: deviceQueryKeys.list(currentUser?.id ?? null),
    queryFn: async (): Promise<DeviceRow[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<DeviceRow[]>("list_user_devices", {
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

export function useRevokeDevice() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (deviceId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("revoke_device", { userId: currentUser.id, deviceId });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: deviceQueryKeys.list(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({ queryKey: securityEventQueryKeys.all });
    },
  });
}
