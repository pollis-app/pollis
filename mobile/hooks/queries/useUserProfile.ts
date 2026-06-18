// Starter hook to seed the mobile data-fetching pattern. Mirrors the
// shape of `frontend/src/hooks/queries/useUserProfile.ts` so that:
//   - Future hooks copy this layout (query key fn, queryFn that calls
//     `invoke()`, store-derived enable flag, staleTime in ms).
//   - When `pollis-native` exposes a real `invoke()` dispatcher, this
//     hook starts returning real data without any call-site change.
//
// Until the bridge is wired (see lib/native/bridge.ts), this hook will
// throw "[pollis-native] invoke('get_user_profile') is not implemented"
// — by design, so the lack of a backend is loud rather than silent.
// Register a mock with `registerMockCommand("get_user_profile", …)` in
// dev to unblock UI work.

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

export interface ServiceUserData {
  username: string;
  preferred_name?: string;
  email: string;
  phone: string;
  avatar_url?: string;
}

export const userQueryKeys = {
  profile: (userId: string | null) => ["user", "profile", userId] as const,
};

export function useUserProfile() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: userQueryKeys.profile(currentUser?.id ?? null),
    queryFn: async (): Promise<ServiceUserData> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      const profile = await invoke<{
        id: string;
        username?: string;
        preferred_name?: string;
        phone?: string;
        avatar_url?: string;
      } | null>("get_user_profile", { userId: currentUser.id });

      return {
        username: profile?.username || currentUser.username || "",
        preferred_name: profile?.preferred_name,
        email: currentUser.email || "",
        phone: profile?.phone || "",
        avatar_url: profile?.avatar_url,
      };
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
    gcTime: 1000 * 60 * 5,
  });
}

export function useUpdateProfile() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setUsername = appStore.setUsername;

  return useMutation({
    mutationFn: async ({
      username,
      preferredName,
      phone,
    }: {
      username: string;
      preferredName?: string;
      phone?: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("update_user_profile", {
        userId: currentUser.id,
        username,
        preferredName: preferredName ?? null,
        phone: phone ?? null,
      });
      return { username };
    },
    onSuccess: (data) => {
      setUsername(data.username);
      queryClient.invalidateQueries({
        queryKey: userQueryKeys.profile(currentUser?.id ?? null),
      });
    },
  });
}
