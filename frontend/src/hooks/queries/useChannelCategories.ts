import { useMutation, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import type { GroupWithChannels } from "../../services/api";
import type { ChannelCategory } from "../../types";
import { appStore } from "../../stores/appStore";
import { groupQueryKeys } from "./useGroups";
import { deleteCategory, moveCategory, moveChannel } from "../../utils/channelLayout";

/*
 * Arranging a group's channels (#2031): admin-only writes on the DS. Each one updates the
 * sidebar tree on the same frame, rolls back if the DS refuses, and re-reads the tree when it
 * lands; other members' sidebars follow through the group-change event.
 */

type Tree = GroupWithChannels[] | undefined;
type Snapshot = { previous: Tree };

/** Apply `edit` to one group of the cached tree, returning the snapshot to roll back to. */
async function editGroup(
  queryClient: QueryClient,
  userId: string | null,
  groupId: string,
  edit: (g: GroupWithChannels) => GroupWithChannels,
): Promise<Snapshot> {
  const key = groupQueryKeys.userGroupsWithChannels(userId);
  await queryClient.cancelQueries({ queryKey: key });
  const previous = queryClient.getQueryData<GroupWithChannels[]>(key);
  queryClient.setQueryData<GroupWithChannels[]>(key, (tree) =>
    tree?.map((g) => (g.id === groupId ? edit(g) : g)),
  );
  return { previous };
}

function useArrangement<V extends { groupId: string }, R>(
  command: string,
  args: (vars: V, requesterId: string) => Record<string, unknown>,
  optimistic: (g: GroupWithChannels, vars: V) => GroupWithChannels,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const userId = currentUser?.id ?? null;
  const key = groupQueryKeys.userGroupsWithChannels(userId);

  return useMutation<R, Error, V, Snapshot>({
    mutationFn: async (vars) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return invoke<R>(command, args(vars, currentUser.id));
    },
    onMutate: (vars) => editGroup(queryClient, userId, vars.groupId, (g) => optimistic(g, vars)),
    onError: (_err, _vars, context) => {
      queryClient.setQueryData(key, context?.previous);
    },
    onSettled: (_data, _err, vars) => {
      queryClient.invalidateQueries({ queryKey: key });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.channels(vars.groupId) });
    },
  });
}

/** A temporary id for a category shown before the DS has answered. */
export const PENDING_CATEGORY_PREFIX = "pending-category-";

export function useCreateCategory() {
  return useArrangement<{ groupId: string; name: string }, ChannelCategory>(
    "create_channel_category",
    ({ groupId, name }, requesterId) => ({ groupId, name, requesterId }),
    (g, { groupId, name }) => ({
      ...g,
      categories: [...g.categories, { id: `${PENDING_CATEGORY_PREFIX}${Date.now()}`, group_id: groupId, name }],
    }),
  );
}

export function useRenameCategory() {
  return useArrangement<{ groupId: string; categoryId: string; name: string }, void>(
    "rename_channel_category",
    ({ groupId, categoryId, name }, requesterId) => ({ groupId, categoryId, name, requesterId }),
    (g, { categoryId, name }) => ({
      ...g,
      categories: g.categories.map((k) => (k.id === categoryId ? { ...k, name } : k)),
    }),
  );
}

export function useDeleteCategory() {
  return useArrangement<{ groupId: string; categoryId: string }, void>(
    "delete_channel_category",
    ({ groupId, categoryId }, requesterId) => ({ groupId, categoryId, requesterId }),
    (g, { categoryId }) => ({ ...g, ...deleteCategory(g.channels, g.categories, categoryId) }),
  );
}

export function useMoveCategory() {
  return useArrangement<{ groupId: string; categoryId: string; position: number }, void>(
    "move_channel_category",
    ({ groupId, categoryId, position }, requesterId) => ({ groupId, categoryId, position, requesterId }),
    (g, { categoryId, position }) => ({ ...g, categories: moveCategory(g.categories, categoryId, position) }),
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
    (g, { channelId, categoryId, position }) => ({
      ...g,
      channels: moveChannel(g.channels, g.categories, channelId, categoryId, position),
    }),
  );
}
