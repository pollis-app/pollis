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
      // The MLS welcome may have already landed in Turso (the inviter
      // queued it when sending the invite). Pull it now so the new group
      // appears in the user's sidebar immediately instead of waiting for
      // the next ingest-on-focus.
      try {
        await invoke("poll_mls_welcomes", { userId: currentUser.id });
      } catch (e) {
        console.warn("[mls] poll_mls_welcomes after accept failed:", e);
      }
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

export function useUpdateGroup(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (vars: {
      name?: string;
      description?: string;
      iconUrl?: string;
    }) => {
      if (!currentUser || !groupId) {
        throw new Error("No active group");
      }
      await invoke("update_group", {
        groupId,
        requesterId: currentUser.id,
        name: vars.name ?? null,
        description: vars.description ?? null,
        iconUrl: vars.iconUrl ?? null,
      });
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

export function useDeleteGroup() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("delete_group", {
        groupId,
        requesterId: currentUser.id,
      });
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

export function useUpdateChannel(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (vars: {
      channelId: string;
      name?: string;
      description?: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("update_channel", {
        channelId: vars.channelId,
        requesterId: currentUser.id,
        name: vars.name ?? null,
        description: vars.description ?? null,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useDeleteChannel(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (channelId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("delete_channel", {
        channelId,
        requesterId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useRemoveMember(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (userId: string) => {
      if (!currentUser || !groupId) {
        throw new Error("No active group");
      }
      await invoke("remove_member_from_group", {
        groupId,
        userId,
        requesterId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupInviteQueryKeys.members(groupId),
      });
    },
  });
}

export function useSetMemberRole(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (vars: { userId: string; role: "admin" | "member" }) => {
      if (!currentUser || !groupId) {
        throw new Error("No active group");
      }
      await invoke("set_member_role", {
        groupId,
        userId: vars.userId,
        role: vars.role,
        requesterId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupInviteQueryKeys.members(groupId),
      });
    },
  });
}

export interface JoinRequest {
  id: string;
  group_id: string;
  requester_id: string;
  requester_username?: string;
  status: "pending" | "approved" | "rejected";
  created_at: string;
}

export const joinRequestQueryKeys = {
  byGroup: (groupId: string | null) => ["join-requests", "group", groupId] as const,
  mine: (groupId: string | null, userId: string | null) =>
    ["join-requests", "mine", groupId, userId] as const,
};

export function useGroupJoinRequests(groupId: string | null) {
  const currentUser = useAppStore((s) => s.currentUser);
  return useQuery({
    queryKey: joinRequestQueryKeys.byGroup(groupId),
    queryFn: async (): Promise<JoinRequest[]> => {
      if (!currentUser || !groupId) {
        return [];
      }
      return await invoke<JoinRequest[]>("get_group_join_requests", {
        groupId,
        requesterId: currentUser.id,
      });
    },
    enabled: !!(currentUser && groupId),
    staleTime: 1000 * 30,
  });
}

export function useMyJoinRequest(groupId: string | null) {
  const currentUser = useAppStore((s) => s.currentUser);
  return useQuery({
    queryKey: joinRequestQueryKeys.mine(groupId, currentUser?.id ?? null),
    queryFn: async (): Promise<JoinRequest | null> => {
      if (!currentUser || !groupId) {
        return null;
      }
      return await invoke<JoinRequest | null>("get_my_join_request", {
        groupId,
        requesterId: currentUser.id,
      });
    },
    enabled: !!(currentUser && groupId),
    staleTime: 1000 * 30,
  });
}

export function useRequestGroupAccess() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("request_group_access", {
        groupId,
        requesterId: currentUser.id,
      });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({
        queryKey: joinRequestQueryKeys.mine(groupId, currentUser?.id ?? null),
      });
    },
  });
}

export function useApproveJoinRequest(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (requestId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("approve_join_request", {
        requestId,
        approverId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: joinRequestQueryKeys.byGroup(groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupInviteQueryKeys.members(groupId),
      });
    },
  });
}

export function useRejectJoinRequest(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (requestId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("reject_join_request", {
        requestId,
        approverId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: joinRequestQueryKeys.byGroup(groupId),
      });
    },
  });
}

export function useGroupBySlug(slug: string | null) {
  return useQuery({
    queryKey: ["group", "slug", slug] as const,
    queryFn: async () => {
      if (!slug) {
        return null;
      }
      return await invoke<{
        id: string;
        slug: string;
        name: string;
        description?: string;
      } | null>("search_group_by_slug", { slug });
    },
    enabled: !!slug && slug.length >= 2,
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
