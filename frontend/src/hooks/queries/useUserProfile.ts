import { useEffect, useMemo } from "react";
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
  /**
   * The SIGNED-IN user's own profile (`ServiceUserData`: username, email,
   * phone, avatar).
   *
   * Deliberately a different key from [`otherProfile`] even for the same user
   * id. The two hooks return different SHAPES from the same command — this one
   * merges in `email` from the session and defaults `username` off the store;
   * the other returns the raw public row with an `id`. They shared one key
   * until #874, which meant opening your own profile page could park the
   * public shape where `useUserProfile` consumers expected the private one and
   * blank out the settings form. Two shapes, two keys.
   */
  profile: (userId: string | null) => ["user", "profile", userId] as const,
  /** Another user's PUBLIC profile row. See [`profile`] for why it is separate. */
  otherProfile: (userId: string | null) => ["user", "public-profile", userId] as const,
};

export function useOtherUserProfile(userId: string | null | undefined) {
  return useQuery({
    queryKey: userQueryKeys.otherProfile(userId ?? null),
    queryFn: async (): Promise<{ id: string; username: string; preferred_name?: string; avatar_url?: string } | null> => {
      if (!userId) {
        return null;
      }
      const profile = await invoke<{ id: string; username?: string; preferred_name?: string; phone?: string; avatar_url?: string } | null>(
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

/** The two flags a shield badge is drawn from. */
export type PeerVerificationState = Pick<
  PeerVerificationEntry,
  "verified" | "key_changed"
>;

/**
 * The same snapshot as `usePeerVerifications`, indexed by peer user id.
 *
 * Every surface that draws a shield next to a person wants the lookup, not the
 * list, and three of them (the sidebar's DM rows, the DMs page, the group
 * member list) each built the identical `useMemo` over it. One shared
 * derivation also means all three cannot drift apart on what "verified" means
 * — which is a security-facing badge, so a divergence there is worse than
 * duplicated code (#874).
 *
 * The `useMemo` still belongs here rather than at the query: the query's data
 * identity is already stable between renders, so memoising on it gives every
 * caller a stable `Map` for free.
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

      const profile = await invoke<{ id: string; username?: string; preferred_name?: string; phone?: string; avatar_url?: string } | null>(
        'get_user_profile',
        { userId: currentUser.id },
      );

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
    refetchOnReconnect: true,
  });

  // Hydrate the denormalized store avatar so the optimistic local voice
  // participant (VoiceSessionManager builds its tile from
  // `store.userAvatarUrl`, and it is not a React consumer) shows the user's
  // avatar. Without it the field stays null until an avatar *update*, so your
  // own voice tile renders the placeholder. Remote tiles are unaffected — they
  // get avatar_url from the backend voice presence (lookup_avatar_url).
  //
  // An EFFECT, not the `queryFn` (#928). A query function runs on refetch, on
  // retry and on cache-restore, with no ordering guarantee against render — so
  // writing observable state from inside it let a background refetch nobody
  // asked for mutate the store, and let a cache HIT (queryFn never runs) leave
  // it unhydrated. Keying on the resolved value instead means the store
  // follows the query's data whatever produced it, and settles at most once
  // per actual change.
  const avatarUrl = query.data?.avatar_url ?? null;
  const hasProfile = query.data !== undefined;
  useEffect(() => {
    if (!hasProfile) {
      return;
    }
    appStore.setUserAvatarUrl(avatarUrl);
  }, [avatarUrl, hasProfile]);

  return query;
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
      // The public view of the same person is now a separate cache entry
      // (#874), so it needs its own invalidation — viewing your own profile
      // page reads that one.
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

/**
 * The signed-in user's own avatar URL.
 *
 * A thin wrapper over [`useAvatarBlobUrl`] since #874 — it used to be a second
 * query, under its own key, fetching the SAME object the `Avatar` component
 * was already fetching. Any screen showing both (the settings page shows the
 * form preview beside the sidebar avatar) downloaded the image twice and
 * cached it twice.
 */
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
      // Avatar keys are content-addressed since #874, so a re-upload produces
      // a NEW key and therefore a new `["avatar-blob", key]` entry — nothing
      // to bust. What still has to happen is the profile refetch above, which
      // is what tells every `Avatar` which key to ask for now.
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
    },
  });
}
