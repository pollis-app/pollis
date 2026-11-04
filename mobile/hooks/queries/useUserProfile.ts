// User profile hooks, mirroring desktop's useUserProfile.ts.

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { AVATAR_DIR, avatarStore } from "../../lib/avatarCache";
import { AVATAR_RENDITIONS } from "../../lib/avatarKey";

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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
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

// Profile picture (desktop: SettingsPage → `upload_public_file` + `update_user_profile`).
// Core does both in `upload_avatar`, re-encoding the photo first (square, no EXIF), and
// writes the new picture's renditions locally so it shows without a download.
export function useSetAvatar() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (path: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      const result = await invoke<{ key: string; uri?: string | null }>("upload_avatar", {
        userId: currentUser.id,
        path,
        px: AVATAR_RENDITIONS[AVATAR_RENDITIONS.length - 1],
        destDir: AVATAR_DIR,
      });
      return { userId: currentUser.id, ...result };
    },
    onSuccess: ({ userId, key, uri }) => {
      avatarStore.adopt(userId, key, uri ?? null);
      appStore.setUserAvatarUrl(key);
      queryClient.setQueryData<ServiceUserData>(userQueryKeys.profile(userId), (prev) =>
        prev ? { ...prev, avatar_url: key } : prev,
      );
      queryClient.invalidateQueries({ queryKey: userQueryKeys.profile(userId) });
    },
  });
}

export function useRemoveAvatar() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    // A removal landed (#1286).
    meta: { haptic: { success: "destructive" } },
    mutationFn: async () => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("remove_avatar", { userId: currentUser.id });
      return currentUser.id;
    },
    onSuccess: (userId) => {
      avatarStore.noteKey(userId, null);
      appStore.setUserAvatarUrl(null);
      queryClient.setQueryData<ServiceUserData>(userQueryKeys.profile(userId), (prev) =>
        prev ? { ...prev, avatar_url: undefined } : prev,
      );
      queryClient.invalidateQueries({ queryKey: userQueryKeys.profile(userId) });
    },
  });
}
