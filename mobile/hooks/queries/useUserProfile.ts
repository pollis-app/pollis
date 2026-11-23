// User profile hooks, mirroring desktop's useUserProfile.ts.

import { useQuery, useMutation, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { AVATAR_DIR, avatarStore } from "../../lib/avatarCache";
import { AVATAR_RENDITIONS } from "../../lib/avatarKey";
import { renameParticipant } from "../../lib/realtime/client";

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

/**
 * #2084: re-read one profile. The read refreshes the core's cached copies of the name
 * (message sender names, typing, and for our own account the accounts index the next launch
 * restores from); here the store, typing names and avatars follow. Run for our own account on
 * landing and every resume, since the session's handle is restored from that index.
 */
export async function refreshProfile(userId: string): Promise<void> {
  try {
    const profile = await invoke<{ id: string; username?: string; avatar_url?: string | null } | null>(
      "get_user_profile",
      { userId },
    );
    if (!profile) {
      return;
    }
    if (profile.username) {
      renameParticipant(userId, profile.username);
      if (appStore.currentUser?.id === userId && appStore.currentUser.username !== profile.username) {
        appStore.setUsername(profile.username);
      }
    }
    avatarStore.noteKey(userId, profile.avatar_url ?? null);
  } catch (err) {
    // Offline: the core re-checks its cache on next use.
    console.warn("[profile] get_user_profile failed:", err);
  }
}

/** Every cached list that names people: profiles, message pages, previews, DMs, groups and members, pins, search. */
const NAMED_QUERY_PREFIXES = [["user"], ["messages"], ["last-message"], ["dm"], ["groups"], ["pins"], ["search"]];

/**
 * #2084: `userId` changed their handle, display name or picture (the DS's content-free
 * `profile_changed`): refresh it, then every surface that names people re-reads.
 * Desktop: `onProfileChanged` in its `useUserProfile.ts`.
 */
export async function onProfileChanged(queryClient: QueryClient, userId: string) {
  await refreshProfile(userId);
  for (const prefix of NAMED_QUERY_PREFIXES) {
    void queryClient.invalidateQueries({ queryKey: prefix });
  }
}

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

export interface ProfileRow {
  id: string;
  username?: string | null;
  preferred_name?: string | null;
  avatar_url?: string | null;
}

/**
 * Many users' profiles in ONE `get_user_profiles` round trip (the batch the avatar
 * store uses); unknown ids are simply absent. Off while `ids` is empty.
 */
export function useUserProfiles(ids: readonly string[]) {
  const sorted = [...new Set(ids)].sort();
  return useQuery({
    queryKey: ["user", "profiles", sorted.join("|")] as const,
    queryFn: async (): Promise<Map<string, ProfileRow>> => {
      const rows = await invoke<ProfileRow[]>("get_user_profiles", { userIds: sorted });
      const map = new Map<string, ProfileRow>();
      for (const row of rows ?? []) {
        map.set(row.id, row);
      }
      return map;
    },
    enabled: sorted.length > 0,
    staleTime: 1000 * 60,
    gcTime: 1000 * 60 * 5,
  });
}
