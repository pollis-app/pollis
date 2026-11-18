import { useObserver } from "mobx-react-lite";
import { useGroupMembers, useUserGroupsWithChannels } from "./queries";
import { isGroupAdmin } from "../lib/identityFields";
import { appStore } from "../stores/appStore";

/**
 * A group page's group: its row, its roster, and whether the viewer is an admin there (roster first,
 * then the group row, so it holds in the moment before the roster loads). Settings, emoji and the
 * Bots pages (#1182) read it the same way.
 */
export function useGroupAccess(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);
  const { data: members = [] } = useGroupMembers(groupId);
  const iAmAdmin = isGroupAdmin(
    members.find((m) => m.user_id === currentUser?.id)?.role,
    group?.current_user_role,
  );
  return { group, members, iAmAdmin };
}
