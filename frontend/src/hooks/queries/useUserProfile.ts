import { useEffect, useMemo } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import * as api from "../../services/api";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { messageQueryKeys } from "./useMessages";
import { groupQueryKeys } from "./useGroups";

/** The signed-in user's own profile; no `phone` here (#929). */
export interface ServiceUserData {
  username: string;
  preferred_name?: string;
  email: string;
  avatar_url?: string;
}

export const userQueryKeys = {
  /** Own profile; a separate key from `otherProfile` since the shapes differ (#874). */
  profile: (userId: string | null) => ["user", "profile", userId] as const,
  /** Another user's PUBLIC profile row. */
  otherProfile: (userId: string | null) => ["user", "public-profile", userId] as const,
};

export function useOtherUserProfile(userId: string | null | undefined) {
  return useQuery({
    queryKey: userQueryKeys.otherProfile(userId ?? null),
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
  /// Both pubkeys, sorted, as `pollis-key:v<n>:<a>:<b>`; the QR content.
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

/// Every TOFU-pinned peer's flags in one round-trip; invalidated on `KeyChanged` and `set_contact_verified`.
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

/** The two flags a shield badge is drawn from. */
export type PeerVerificationState = Pick<
  PeerVerificationEntry,
  "verified" | "key_changed"
>;

/** `usePeerVerifications` indexed by peer id, shared so the security badge can't drift (#874). */
export function usePeerVerificationMap(): Map<string, PeerVerificationState> {
  const { data: peerVerifications = [] } = usePeerVerifications();
  return useMemo(() => {
    const map = new Map<string, PeerVerificationState>();
    for (const entry of peerVerifications) {
      map.set(entry.peer_user_id, {
        verified: entry.verified,
        key_changed: entry.key_changed,
      });
    }
    return map;
  }, [peerVerifications]);
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

  const query = useQuery({
    queryKey: userQueryKeys.profile(currentUser?.id ?? null),
    queryFn: async (): Promise<ServiceUserData> => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      const profile = await invoke<{ id: string; username?: string; preferred_name?: string; avatar_url?: string } | null>(
        'get_user_profile',
        { userId: currentUser.id },
      );

      return {
        username: profile?.username || currentUser.username || '',
        preferred_name: profile?.preferred_name,
        email: currentUser.email || '',
        avatar_url: profile?.avatar_url,
      };
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
    gcTime: 1000 * 60 * 5,
    refetchOnReconnect: true,
  });

  // Hydrate the store avatar for the local voice tile; an effect, not the queryFn (#928).
  const avatarUrl = query.data?.avatar_url ?? null;
  const hasProfile = query.data !== undefined;
  useEffect(() => {
    if (!hasProfile) {
      return;
    }
    appStore.setUserAvatarUrl(avatarUrl);
  }, [avatarUrl, hasProfile]);

  // The accounts index is a cache; the profile query is the truth.
  const username = query.data?.username || null;
  useEffect(() => {
    if (!hasProfile || !username || appStore.currentUser?.username === username) {
      return;
    }
    appStore.setUsername(username);
  }, [username, hasProfile]);

  return query;
}

export function useUpdateProfile() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setUsername = appStore.setUsername;

  return useMutation({
    mutationFn: async ({ username, preferredName }: { username: string; preferredName?: string }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      await api.updateUserProfile(currentUser.id, username, preferredName);
      return { username };
    },
    onSuccess: (data) => {
      setUsername(data.username);
      queryClient.invalidateQueries({
        queryKey: userQueryKeys.profile(currentUser?.id ?? null),
      });
      // The public view is a separate entry (#874).
      queryClient.invalidateQueries({
        queryKey: userQueryKeys.otherProfile(currentUser?.id ?? null),
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

/** The signed-in user's avatar URL, via `useAvatarBlobUrl` (#874). */
export function useUserAvatar() {
  const { data: userProfile } = useUserProfile();
  return useAvatarBlobUrl(userProfile?.avatar_url ?? null);
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
        queryKey: userQueryKeys.otherProfile(currentUser?.id ?? null),
      });
      // Avatar keys are content-addressed (#874); the profile refetch is enough.
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
    },
  });
}
