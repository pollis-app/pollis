// Device list / revoke hooks for the Security screen. Backed by
// `list_user_devices` and `revoke_device`.
//
// The Rust side blocks revoking the current device — `logout(delete_data:
// true)` is the path for "sign out of this device" — so the UI's revoke
// button is hidden on the row marked `is_current`.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

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
    },
  });
}
