// DM listing hooks. Mirrors the read paths of
// `frontend/src/hooks/queries/useMessages.ts::useDMConversations` —
// transforms the Rust `DmChannel` shape into the mobile UI's lighter
// `DMConversation` view-model, picking the "other side" relative to the
// currently signed-in user.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { DMConversation } from "../../types";

interface RawDmMember {
  user_id: string;
  username?: string;
  avatar_url?: string;
}

interface RawDmChannel {
  id: string;
  members: RawDmMember[];
  created_at: string;
}

export const dmQueryKeys = {
  all: ["dm"] as const,
  channels: (userId: string | null) => ["dm", "channels", userId] as const,
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

export function useAcceptDMRequest() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (dmChannelId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("accept_dm_request", {
        dmChannelId,
        userId: currentUser.id,
      });
      // The other side queued an MLS welcome for us when they opened
      // the DM. Pull it now so the conversation appears as fully-keyed
      // immediately instead of after the next ingest.
      try {
        await invoke("poll_mls_welcomes", { userId: currentUser.id });
      } catch (e) {
        console.warn("[mls] poll_mls_welcomes after dm accept failed:", e);
      }
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
