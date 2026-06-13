import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import * as api from "../../services/api";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { messageQueryKeys } from "./useMessages";
import { groupQueryKeys } from "./useGroups";

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

export function useOtherUserProfile(userId: string | null | undefined) {
  return useQuery({
    queryKey: userQueryKeys.profile(userId ?? null),
    queryFn: async (): Promise<{ id: string; username: string; preferred_name?: string; avatar_url?: string } | null> => {
      if (!userId) {
        return null;
      }
      const profile = await invoke<{ id: string; username?: string; preferred_name?: string; avatar_url?: string } | null>(
        'get_user_profile',
        { userId },
      );
      if (!profile) {
        return null;
      }
      return {
        id: profile.id,
        username: profile.username ?? '',
        preferred_name: profile.preferred_name,
        avatar_url: profile.avatar_url,
      };
    },
    enabled: !!userId,
    staleTime: 1000 * 60,
    gcTime: 1000 * 60 * 5,
  });
}

export interface SafetyNumberInfo {
  safety_number: string;
  status: "unverified" | "verified" | "changed";
  peer_identity_version: number;
  /// Both pubkeys joined as `pollis-key:v<n>:<a>:<b>`, sorted so both
  /// sides scan the same string. Encoded directly into the QR code.
  qr_payload: string;
}

export interface PeerVerificationEntry {
  peer_user_id: string;
  verified: boolean;
  key_changed: boolean;
}

export const peerVerificationKeys = {
  all: ["safety", "peer-verifications"] as const,
};

/// Snapshot of every TOFU-pinned peer plus their verified/key_changed
/// flags. Single round-trip — used for shield-badge rendering across the
/// sidebar / DM list and for the inline key-changed banner. Invalidated
/// on the `KeyChanged` realtime event and after `set_contact_verified`.
export function usePeerVerifications() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: peerVerificationKeys.all,
    queryFn: async (): Promise<PeerVerificationEntry[]> => {
      return await invoke<PeerVerificationEntry[]>("list_peer_verifications");
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

export const safetyQueryKeys = {
  number: (peerUserId: string | null) => ["safety", "number", peerUserId] as const,
};

export function useSafetyNumber(peerUserId: string | null | undefined) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: safetyQueryKeys.number(peerUserId ?? null),
    queryFn: async (): Promise<SafetyNumberInfo> => {
      return await invoke<SafetyNumberInfo>("get_safety_number", {
        myUserId: currentUser!.id,
        peerUserId,
      });
    },
    enabled: !!peerUserId && !!currentUser && currentUser.id !== peerUserId,
    staleTime: 1000 * 30,
  });
}

export function useSetContactVerified(peerUserId: string | null | undefined) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (verified: boolean) => {
      await invoke("set_contact_verified", { peerUserId, verified });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: safetyQueryKeys.number(peerUserId ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: peerVerificationKeys.all,
      });
    },
  });
}

export function useUserProfile() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: userQueryKeys.profile(currentUser?.id ?? null),
    queryFn: async (): Promise<ServiceUserData> => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      const profile = await invoke<{ id: string; username?: string; preferred_name?: string; phone?: string; avatar_url?: string } | null>(
        'get_user_profile',
        { userId: currentUser.id },
      );

      // Hydrate the denormalized store avatar so the optimistic local voice
      // participant (VoiceSessionManager builds its tile from
      // `store.userAvatarUrl`) shows the user's avatar. Without this it stays
      // null until an avatar *update*, so your own voice tile renders the
      // placeholder. Remote tiles are unaffected — they get avatar_url from
      // the backend voice presence (lookup_avatar_url).
      appStore.setUserAvatarUrl(profile?.avatar_url ?? null);

      return {
        username: profile?.username || currentUser.username || '',
        preferred_name: profile?.preferred_name,
        email: currentUser.email || '',
        phone: profile?.phone || '',
        avatar_url: profile?.avatar_url,
      };
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
    gcTime: 1000 * 60 * 5,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
  });
}

export function useUpdateProfile() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setUsername = appStore.setUsername;

  return useMutation({
    mutationFn: async ({ username, preferredName, phone }: { username: string; preferredName?: string; phone?: string }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      await api.updateUserProfile(currentUser.id, username, preferredName, phone);
      return { username };
    },
    onSuccess: (data) => {
      setUsername(data.username);
      // Username appears in messages, DM previews, and group membership — invalidate
      // all of these so the updated name is reflected everywhere.
      queryClient.invalidateQueries({
        queryKey: userQueryKeys.profile(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.all,
      });
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useUserAvatar() {
  const { data: userProfile } = useUserProfile();
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: ["user", "avatar", currentUser?.id, userProfile?.avatar_url],
    queryFn: async (): Promise<string | null> => {
      if (!userProfile?.avatar_url) {
        return null;
      }
      const { getFileDownloadUrl } = await import("../../services/r2-upload");
      return await getFileDownloadUrl(userProfile.avatar_url);
    },
    enabled: !!currentUser && !!userProfile?.avatar_url,
    staleTime: 1000 * 60 * 30,
    gcTime: 1000 * 60 * 60,
    retry: 1,
  });
}

export function useAvatarBlobUrl(avatarKey: string | null | undefined) {
  return useQuery({
    queryKey: ["avatar-blob", avatarKey ?? null],
    queryFn: async (): Promise<string | null> => {
      if (!avatarKey) {
        return null;
      }
      const { getFileDownloadUrl } = await import("../../services/r2-upload");
      return await getFileDownloadUrl(avatarKey);
    },
    enabled: !!avatarKey,
    staleTime: 1000 * 60 * 30,
    gcTime: 1000 * 60 * 60,
    retry: 1,
  });
}

export function useUpdateAvatar() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setUserAvatarUrl = appStore.setUserAvatarUrl;

  return useMutation({
    mutationFn: async (avatarUrl: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      await api.updateUserProfile(currentUser.id, undefined, undefined, undefined, avatarUrl);
      return avatarUrl;
    },
    onSuccess: (avatarUrl) => {
      setUserAvatarUrl(avatarUrl);
      queryClient.invalidateQueries({
        queryKey: userQueryKeys.profile(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: ["user", "avatar", currentUser?.id],
      });
      // Because the R2 key is stable per user (`avatars/{userId}`), the
      // blob-url cache key doesn't change after re-upload — invalidate it
      // explicitly so a fresh download_file fires and the new bytes render.
      queryClient.invalidateQueries({
        queryKey: ["avatar-blob", avatarUrl],
      });
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
    },
  });
}
