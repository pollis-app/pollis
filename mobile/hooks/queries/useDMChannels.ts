// Mirrors the read paths of desktop's `useDMConversations`.

import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { DMConversation } from "../../types";

export interface RawDmMember {
  user_id: string;
  username?: string;
  avatar_url?: string;
}

export interface RawDmChannel {
  id: string;
  created_by?: string;
  members: RawDmMember[];
  created_at: string;
}

export const dmQueryKeys = {
  all: ["dm"] as const,
  channels: (userId: string | null) => ["dm", "channels", userId] as const,
  channel: (dmChannelId: string | null) => ["dm", "channel", dmChannelId] as const,
  requests: (userId: string | null) => ["dm", "requests", userId] as const,
};

function transform(
  raw: RawDmChannel,
  selfId: string,
): DMConversation {
  const other = raw.members.find((m) => m.user_id !== selfId);
  const ts = new Date(raw.created_at).getTime();
  return {
    id: raw.id,
    user1_id: selfId,
    user2_identifier: other?.username || other?.user_id || "Unknown",
    user2_id: other?.user_id,
    user2_avatar_url: other?.avatar_url,
    created_at: ts,
    updated_at: ts,
  };
}

export function useDMChannels() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: dmQueryKeys.channels(currentUser?.id ?? null),
    queryFn: async (): Promise<DMConversation[]> => {
      if (!currentUser) {
        return [];
      }
      const channels = await invoke<RawDmChannel[]>("list_dm_channels", {
        userId: currentUser.id,
      });
      return (channels ?? []).map((c) => transform(c, currentUser.id));
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

/**
 * One DM channel with its full roster, from the source the list rows come
 * from (#907), so roster and list never disagree. Keyed under `["dm"]`, so the
 * inbox realtime's invalidations refresh it too.
 */
export function useDMChannel(dmChannelId: string | null) {
  return useQuery({
    queryKey: dmQueryKeys.channel(dmChannelId),
    queryFn: async (): Promise<RawDmChannel | null> => {
      if (!dmChannelId) {
        return null;
      }
      return await invoke<RawDmChannel>("get_dm_channel", { dmChannelId });
    },
    enabled: !!dmChannelId,
    staleTime: 1000 * 60,
  });
}

/**
 * Drop matching rows from the cached message requests on the tap (#1561), so
 * the row and the Direct badge go at once; a failure refetches to restore it.
 */
export async function dropDMRequests(
  queryClient: QueryClient,
  userId: string | null,
  match: (request: DMConversation) => boolean,
): Promise<void> {
  const key = dmQueryKeys.requests(userId);
  await queryClient.cancelQueries({ queryKey: key });
  queryClient.setQueryData<DMConversation[]>(key, (prev) => prev?.filter((r) => !match(r)));
}

export function useAcceptDMRequest() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    meta: { haptic: { success: "success" } },
    mutationFn: async (dmChannelId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("accept_dm_request", {
        dmChannelId,
        userId: currentUser.id,
      });
      // Pull the welcome the other side queued, so the conversation is keyed
      // now rather than after the next ingest.
      try {
        await invoke("poll_mls_welcomes", { userId: currentUser.id });
      } catch (e) {
        console.warn("[mls] poll_mls_welcomes after dm accept failed:", e);
      }
    },
    onMutate: async (dmChannelId: string) => {
      await dropDMRequests(queryClient, currentUser?.id ?? null, (r) => r.id === dmChannelId);
    },
    onError: () => {
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.requests(currentUser?.id ?? null),
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.requests(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.channels(currentUser?.id ?? null),
      });
    },
  });
}

export function useLeaveDM() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (dmChannelId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("leave_dm_channel", {
        dmChannelId,
        userId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.channels(currentUser?.id ?? null),
      });
    },
  });
}

export function useCreateDM() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    meta: { haptic: {} },
    mutationFn: async (vars: { memberIds: string[] }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      const raw = await invoke<RawDmChannel>("create_dm_channel", {
        creatorId: currentUser.id,
        memberIds: vars.memberIds,
      });
      return transform(raw, currentUser.id);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.channels(currentUser?.id ?? null),
      });
    },
  });
}

export function useDMRequests() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: dmQueryKeys.requests(currentUser?.id ?? null),
    queryFn: async (): Promise<DMConversation[]> => {
      if (!currentUser) {
        return [];
      }
      const channels = await invoke<RawDmChannel[]>("list_dm_requests", {
        userId: currentUser.id,
      });
      return (channels ?? []).map((c) => transform(c, currentUser.id));
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}
