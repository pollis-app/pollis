// Pending group-invite hooks for the receiving (invitee) side, plus the
// send-side mutation for the inviter (used from group/[id]/invite). The
// member-list + leave-group hooks live here too because they all sit on
// the group detail screen and share invalidation patterns.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { useAppStore } from "../../stores/appStore";
import { groupQueryKeys } from "./useUserGroups";

export interface PendingInvite {
  id: string;
  group_id: string;
  group_name: string;
  inviter_id: string;
  inviter_username?: string;
  created_at: string;
}

export interface GroupMember {
  user_id: string;
  username?: string;
  avatar_url?: string;
  role: string;
  joined_at: string;
}

export const groupInviteQueryKeys = {
  pending: (userId: string | null) => ["group-invites", "pending", userId] as const,
  members: (groupId: string | null) => ["groups", groupId, "members"] as const,
};

export function usePendingGroupInvites() {
  const currentUser = useAppStore((s) => s.currentUser);
  return useQuery({
    queryKey: groupInviteQueryKeys.pending(currentUser?.id ?? null),
    queryFn: async (): Promise<PendingInvite[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<PendingInvite[]>("get_pending_invites", {
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

export function useAcceptGroupInvite() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (inviteId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("accept_group_invite", { inviteId, userId: currentUser.id });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupInviteQueryKeys.pending(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useDeclineGroupInvite() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (inviteId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("decline_group_invite", { inviteId, userId: currentUser.id });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupInviteQueryKeys.pending(currentUser?.id ?? null),
      });
    },
  });
}

export function useSendGroupInvite(groupId: string | null) {
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (inviteeIdentifier: string) => {
      if (!currentUser || !groupId) {
        throw new Error("No active group");
      }
      await invoke("send_group_invite", {
        groupId,
        inviterId: currentUser.id,
        inviteeIdentifier,
      });
    },
  });
}

export function useGroupMembers(groupId: string | null) {
  return useQuery({
    queryKey: groupInviteQueryKeys.members(groupId),
    queryFn: async (): Promise<GroupMember[]> => {
      if (!groupId) {
        return [];
      }
      return await invoke<GroupMember[]>("get_group_members", { groupId });
    },
    enabled: !!groupId,
    staleTime: 1000 * 60,
  });
}

export function useLeaveGroup() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("leave_group", { groupId, userId: currentUser.id });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}
