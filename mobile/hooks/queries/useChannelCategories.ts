// Arranging a group's channels (#2031), mirroring desktop's useChannelCategories.ts: admin-only
// writes on the DS that update the cached lists on the same frame, roll back if the DS refuses,
// and re-read when they land. Other members follow through the group-change event.

import { useMutation, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import type { Channel, ChannelCategory } from "../../types";
import { deleteCategory, moveCategory, moveChannel } from "../../lib/channelLayout";
import type { MutationHaptic } from "../../lib/hapticEvents";
import { groupQueryKeys, type GroupWithChannels } from "./useUserGroups";

type Snapshot = { tree: GroupWithChannels[] | undefined; channels: Channel[] | undefined };

/** The arranged form of one group: every channel (voice too) and the categories. */
interface Arranged {
  all: Channel[];
  categories: ChannelCategory[];
}

const textOnly = (all: Channel[]) => all.filter((c) => c.channel_type !== "voice");

async function editGroup(
  queryClient: QueryClient,
  userId: string | null,
  groupId: string,
  edit: (a: Arranged) => Arranged,
): Promise<Snapshot> {
  const treeKey = groupQueryKeys.userGroupsWithChannels(userId);
  const channelsKey = groupQueryKeys.channels(groupId);
  await Promise.all([
    queryClient.cancelQueries({ queryKey: treeKey }),
    queryClient.cancelQueries({ queryKey: channelsKey }),
  ]);
  const snapshot: Snapshot = {
    tree: queryClient.getQueryData<GroupWithChannels[]>(treeKey),
    channels: queryClient.getQueryData<Channel[]>(channelsKey),
  };
  const group = snapshot.tree?.find((g) => g.id === groupId);
  if (!group) {
    return snapshot;
  }
  const next = edit({ all: group.allChannels ?? group.channels, categories: group.categories ?? [] });
  queryClient.setQueryData<GroupWithChannels[]>(treeKey, (tree) =>
    tree?.map((g) =>
      g.id === groupId
        ? { ...g, allChannels: next.all, channels: textOnly(next.all), categories: next.categories }
        : g,
    ),
  );
  // The panel prefers the group's own channel read; keep it in step.
  if (snapshot.channels) {
    queryClient.setQueryData<Channel[]>(channelsKey, textOnly(next.all));
  }
  return snapshot;
}

function useArrangement<V extends { groupId: string }, R>(
  command: string,
  args: (vars: V, requesterId: string) => Record<string, unknown>,
  optimistic: (a: Arranged, vars: V) => Arranged,
  haptic: MutationHaptic<V>,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const userId = currentUser?.id ?? null;

  return useMutation<R, Error, V, Snapshot>({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic },
    mutationFn: async (vars) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return invoke<R>(command, args(vars, currentUser.id));
    },
    onMutate: (vars) => editGroup(queryClient, userId, vars.groupId, (a) => optimistic(a, vars)),
    onError: (_err, vars, context) => {
      queryClient.setQueryData(groupQueryKeys.userGroupsWithChannels(userId), context?.tree);
      if (context?.channels) {
        queryClient.setQueryData(groupQueryKeys.channels(vars.groupId), context.channels);
      }
    },
    onSettled: (_data, _err, vars) => {
      void queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(userId) });
      void queryClient.invalidateQueries({ queryKey: groupQueryKeys.channels(vars.groupId) });
    },
  });
}

export function useCreateCategory() {
  return useArrangement<{ groupId: string; name: string }, ChannelCategory>(
    "create_channel_category",
    ({ groupId, name }, requesterId) => ({ groupId, name, requesterId }),
    (a, { groupId, name }) => ({
      ...a,
      categories: [...a.categories, { id: `pending-category-${Date.now()}`, group_id: groupId, name }],
    }),
    { success: "success" },
  );
}

export function useRenameCategory() {
  return useArrangement<{ groupId: string; categoryId: string; name: string }, void>(
    "rename_channel_category",
    ({ groupId, categoryId, name }, requesterId) => ({ groupId, categoryId, name, requesterId }),
    (a, { categoryId, name }) => ({
      ...a,
      categories: a.categories.map((k) => (k.id === categoryId ? { ...k, name } : k)),
    }),
    { success: "success" },
  );
}

export function useDeleteCategory() {
  return useArrangement<{ groupId: string; categoryId: string }, void>(
    "delete_channel_category",
    ({ groupId, categoryId }, requesterId) => ({ groupId, categoryId, requesterId }),
    (a, { categoryId }) => {
      const next = deleteCategory(a.all, a.categories, categoryId);
      return { all: next.channels, categories: next.categories };
    },
    { success: "destructive" },
  );
}

export function useMoveCategory() {
  return useArrangement<{ groupId: string; categoryId: string; position: number }, void>(
    "move_channel_category",
    ({ groupId, categoryId, position }, requesterId) => ({ groupId, categoryId, position, requesterId }),
    (a, { categoryId, position }) => ({ ...a, categories: moveCategory(a.categories, categoryId, position) }),
    { mutate: "toggle" },
  );
}

export function useMoveChannel() {
  return useArrangement<{ groupId: string; channelId: string; categoryId: string | null; position: number }, void>(
    "move_channel",
    ({ groupId, channelId, categoryId, position }, requesterId) => ({
      groupId,
      channelId,
      categoryId,
      position,
      requesterId,
    }),
    (a, { channelId, categoryId, position }) => ({
      ...a,
      all: moveChannel(a.all, a.categories, channelId, categoryId, position),
    }),
    { mutate: "toggle" },
  );
}
