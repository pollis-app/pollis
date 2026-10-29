import { useEffect, useMemo } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import * as api from "../../services/api";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { messageQueryKeys } from "./useMessages";
import { groupQueryKeys } from "./useGroups";

/**
 * The signed-in user's profile as the settings form needs it. No `phone`
 * (#929): only mobile surfaces it.
 */
export interface ServiceUserData {
  username: string;
  preferred_name?: string;
  email: string;
  avatar_url?: string;
}

export const userQueryKeys = {
  /**
   * The SIGNED-IN user's own profile. A different key from [`otherProfile`]
   * even for the same id: the two return different shapes from one command
   * (#874).
   */
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
  /// Both pubkeys as `pollis-key:v<n>:<a>:<b>`, sorted so both sides scan the
  /// same string; the QR payload.
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

/// Every TOFU-pinned peer with verified/key_changed flags, in one round-trip.
/// Invalidated on `KeyChanged` and after `set_contact_verified`.
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

export type PeerVerificationState = Pick<
  PeerVerificationEntry,
  "verified" | "key_changed"
>;

/**
 * `usePeerVerifications` indexed by peer id: one shared derivation so the
 * security badge can't drift between surfaces (#874).
 */
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

  // Hydrate the store avatar, which the optimistic local voice tile reads
  // (not a React consumer). In an effect, not the `queryFn` (#928): a refetch
  // must not mutate the store, and a cache hit must still hydrate it.
  const avatarUrl = query.data?.avatar_url ?? null;
  const hasProfile = query.data !== undefined;
  useEffect(() => {
    if (!hasProfile) {
      return;
    }
    appStore.setUserAvatarUrl(avatarUrl);
  }, [avatarUrl, hasProfile]);

  // Same for the username: `get_session` seeds it from the accounts index, a
  // cache that can be stale; the profile is the remote truth.
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
      // The name shows in messages, previews and membership.
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

/** The signed-in user's avatar URL, via [`useAvatarBlobUrl`] so it shares `Avatar`'s fetch (#874). */
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
      // Avatar keys are content-addressed (#874): a re-upload is a new key, so
      // only the profile refetch is needed.
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
    },
  });
}
