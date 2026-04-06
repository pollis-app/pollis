import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import * as api from "../../services/api";
import type { GroupWithChannels } from "../../services/api";
import { useAppStore } from "../../stores/appStore";
import type { Group, Channel, GroupMember } from "../../types";

export const groupQueryKeys = {
  all: ["groups"] as const,
  userGroups: (userId: string | null) => ["groups", "user", userId] as const,
  userGroupsWithChannels: (userId: string | null) => ["groups", "with-channels", userId] as const,
  group: (groupId: string) => ["groups", groupId] as const,
  channels: (groupId: string) => ["groups", groupId, "channels"] as const,
  members: (groupId: string) => ["groups", groupId, "members"] as const,
  pendingInvites: (userId: string | null) => ["group-invites", "pending", userId] as const,
  joinRequests: (groupId: string) => ["group-join-requests", groupId] as const,
};

export function useUserGroupsWithChannels() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
    queryFn: async (): Promise<GroupWithChannels[]> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await api.listUserGroupsWithChannels(currentUser.id);
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
    refetchOnWindowFocus: true,
  });
}

export function useUserGroups() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
    queryFn: async (): Promise<Group[]> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await api.listUserGroups(currentUser.id);
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
    refetchOnWindowFocus: true,
  });
}

export function useGroupChannels(groupId: string | null) {
  return useQuery({
    queryKey: groupQueryKeys.channels(groupId ?? ""),
    queryFn: async (): Promise<Channel[]> => {
      if (!groupId) {
        throw new Error("No group ID provided");
      }
      return await api.listChannels(groupId);
    },
    enabled: !!groupId,
    staleTime: 1000 * 60,
    refetchOnWindowFocus: true,
  });
}

export function useCreateGroup() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);
  const setGroups = useAppStore((state) => state.setGroups);

  return useMutation({
    mutationFn: async ({
      name,
      description,
    }: {
      slug: string;
      name: string;
      description: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      return await invoke<{ id: string; name: string; description?: string; owner_id: string; created_at: string }>(
        'create_group',
        { name, description: description || null, ownerId: currentUser.id },
      );
    },
    onSuccess: (rawGroup) => {
      const ts = new Date(rawGroup.created_at).getTime();
      const newGroup: Group = {
        id: rawGroup.id,
        slug: '',
        name: rawGroup.name,
        description: rawGroup.description || '',
        created_by: rawGroup.owner_id,
        created_at: ts,
        updated_at: ts,
      };

      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });

      queryClient.setQueryData<Group[]>(
        groupQueryKeys.userGroups(currentUser?.id ?? null),
        (oldGroups) => {
          const updated = [...(oldGroups || []), newGroup];
          setGroups(updated);
          return updated;
        },
      );
    },
  });
}

export function useJoinGroup() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      await invoke('invite_to_group', { groupId, userId: currentUser.id });
      return { id: groupId, slug: groupId, name: groupId, description: '', created_by: currentUser.id, created_at: 0, updated_at: 0 } as Group;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
    },
  });
}

export function useUpdateGroupIcon() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);
  const setGroups = useAppStore((state) => state.setGroups);

  return useMutation({
    mutationFn: async ({ groupId, iconUrl }: { groupId: string; iconUrl: string }) => {
      await api.updateGroupIcon(groupId, iconUrl);
      return { groupId, iconUrl };
    },
    onSuccess: ({ groupId, iconUrl }) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });

      queryClient.setQueryData<Group[]>(
        groupQueryKeys.userGroups(currentUser?.id ?? null),
        (oldGroups) => {
          if (!oldGroups) {
            return oldGroups;
          }
          const updated = oldGroups.map((g) =>
            g.id === groupId ? { ...g, icon_url: iconUrl } : g,
          );
          setGroups(updated);
          return updated;
        },
      );
    },
  });
}

export function useCreateChannel() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);
  const setChannels = useAppStore((state) => state.setChannels);

  return useMutation({
    mutationFn: async ({
      groupId,
      name,
      description,
    }: {
      groupId: string;
      slug: string;
      name: string;
      description: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      return await invoke<{ id: string; group_id: string; name: string; description?: string }>(
        'create_channel',
        { groupId, name, description: description || null, creatorId: currentUser.id },
      );
    },
    onSuccess: (rawChannel, variables) => {
      const newChannel: Channel = {
        id: rawChannel.id,
        group_id: rawChannel.group_id,
        slug: '',
        name: rawChannel.name,
        description: rawChannel.description || '',
        channel_type: 'text',
        created_by: currentUser?.id || '',
        created_at: 0,
        updated_at: 0,
      };

      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(variables.groupId),
      });
      // Also invalidate the combined groups+channels query used by the sidebar
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });

      queryClient.setQueryData<Channel[]>(
        groupQueryKeys.channels(variables.groupId),
        (oldChannels) => {
          const updated = [...(oldChannels || []), newChannel];
          setChannels(variables.groupId, updated);
          return updated;
        },
      );
    },
  });
}

export function useLeaveGroup() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

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

export type PendingInvite = {
  id: string;
  group_id: string;
  group_name: string;
  inviter_id: string;
  inviter_username?: string;
  created_at: string;
};

export type JoinRequest = {
  id: string;
  group_id: string;
  requester_id: string;
  requester_username?: string;
  created_at: string;
};

export function usePendingInvites() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.pendingInvites(currentUser?.id ?? null),
    queryFn: async (): Promise<PendingInvite[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<PendingInvite[]>('get_pending_invites', { userId: currentUser.id });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
}

export function useAcceptInvite() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (inviteId: string) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('accept_group_invite', { inviteId, userId: currentUser.id });
      // Drain MLS Welcome messages the inviter pre-generated so we can decrypt
      // channel messages immediately after navigating into the group.
      await invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
        console.warn('[mls] poll_mls_welcomes after accept:', err);
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.pendingInvites(currentUser?.id ?? null) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null) });
    },
  });
}

export function useDeclineInvite() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (inviteId: string) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('decline_group_invite', { inviteId, userId: currentUser.id });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.pendingInvites(currentUser?.id ?? null) });
    },
  });
}

export function useGroupMembers(groupId: string | null) {
  return useQuery({
    queryKey: groupQueryKeys.members(groupId ?? ''),
    queryFn: async (): Promise<GroupMember[]> => {
      if (!groupId) {
        return [];
      }
      return await invoke<GroupMember[]>('get_group_members', { groupId });
    },
    enabled: !!groupId,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
}

export function useSetMemberRole() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, userId, role }: { groupId: string; userId: string; role: 'admin' | 'member' }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('set_member_role', { groupId, userId, role, requesterId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.members(groupId) });
    },
  });
}

export function useKickMember() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, userId }: { groupId: string; userId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('remove_member_from_group', { groupId, userId, requesterId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.members(groupId) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null) });
    },
  });
}

export function useRequestGroupAccess() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('request_group_access', { groupId, requesterId: currentUser.id });
    },
  });
}

export function useSendGroupInvite() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, inviteeIdentifier }: { groupId: string; inviteeIdentifier: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('send_group_invite', { groupId, inviterId: currentUser.id, inviteeIdentifier });
    },
  });
}

export function useGroupJoinRequests(groupId: string | null) {
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.joinRequests(groupId ?? ''),
    queryFn: async (): Promise<JoinRequest[]> => {
      if (!currentUser || !groupId) {
        return [];
      }
      return await invoke<JoinRequest[]>('get_group_join_requests', { groupId, requesterId: currentUser.id });
    },
    enabled: !!currentUser && !!groupId,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
}

export function useApproveJoinRequest() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async ({ requestId, groupId }: { requestId: string; groupId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('approve_join_request', { requestId, approverId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.joinRequests(groupId) });
    },
  });
}

export function useRejectJoinRequest() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async ({ requestId, groupId }: { requestId: string; groupId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('reject_join_request', { requestId, approverId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.joinRequests(groupId) });
    },
  });
}

export function useAllPendingJoinRequests() {
  const currentUser = useAppStore((state) => state.currentUser);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();

  const adminGroupIds = useMemo(() => {
    if (!currentUser || !groupsWithChannels) {
      return [];
    }
    return groupsWithChannels
      .filter((g) => g.current_user_role === 'admin')
      .map((g) => g.id);
  }, [currentUser?.id, groupsWithChannels]);

  return useQuery({
    queryKey: ["join-requests", "all-admin", currentUser?.id ?? null, adminGroupIds.length],
    queryFn: async (): Promise<JoinRequest[]> => {
      if (!currentUser || adminGroupIds.length === 0) {
        return [];
      }
      const results = await Promise.all(
        adminGroupIds.map((groupId) =>
          invoke<JoinRequest[]>('get_group_join_requests', { groupId, requesterId: currentUser.id }),
        ),
      );
      return results.flat();
    },
    enabled: !!currentUser && adminGroupIds.length > 0,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
}
