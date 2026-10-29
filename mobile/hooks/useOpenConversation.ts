// Opening a conversation or group from outside its tab. Phones push
// `chat/[id]` / `group/[id]`; iPad selects it in the store and returns to the
// tab, whose two-pane renders the selection.

import { useCallback } from "react";
import { useRouter, useSegments, type Href } from "expo-router";
import { useIsRegular } from "./useLayoutClass";
import { markConversationRead, useUserGroupsWithChannels } from "./queries";
import { appStore } from "../stores/appStore";

export type ConversationTarget = {
  id: string;
  kind: "channel" | "dm";
  name?: string;
  // The channel's group, when the caller knows it; looked up otherwise.
  groupId?: string;
};

type Mode = { replace?: boolean };

// Returns the tab route.
export function selectConversation(
  target: ConversationTarget,
  groupIdForChannel: string | null,
): "/(tabs)/groups" | "/(tabs)/direct" {
  if (target.kind === "dm") {
    appStore.setSelectedConversationId(target.id);
    markConversationRead(target.id);
    return "/(tabs)/direct";
  }
  if (groupIdForChannel && groupIdForChannel !== appStore.selectedGroupId) {
    appStore.setSelectedGroupId(groupIdForChannel);
  }
  appStore.setSelectedChannelId(target.id);
  markConversationRead(target.id);
  return "/(tabs)/groups";
}

export function useOpenConversation() {
  const router = useRouter();
  const regular = useIsRegular();
  const { data: groups } = useUserGroupsWithChannels();
  // Already inside the tab navigator (Search, a pane page): switch tabs.
  // From a screen pushed over the tabs: pop back down to them.
  const segments = useSegments();
  const inTabs = segments[0] === "(tabs)";
  const goToTab = useCallback(
    (tab: "/(tabs)/groups" | "/(tabs)/direct") => {
      if (inTabs) {
        router.navigate(tab as Href);
      } else {
        router.dismissTo(tab as Href);
      }
    },
    [inTabs, router],
  );

  const openConversation = useCallback(
    (target: ConversationTarget, mode: Mode = {}) => {
      if (regular) {
        const groupId =
          target.groupId ??
          groups?.find((g) => g.channels.some((c) => c.id === target.id))?.id ??
          null;
        goToTab(selectConversation(target, groupId));
        return;
      }
      // The chat screen joins a channel's realtime room by the selected group,
      // which a caller from outside the group has not set.
      if (target.kind === "channel") {
        const groupId =
          target.groupId ??
          groups?.find((g) => g.channels.some((c) => c.id === target.id))?.id ??
          null;
        if (groupId && groupId !== appStore.selectedGroupId) {
          appStore.setSelectedGroupId(groupId);
        }
      }
      const href = {
        pathname: "/chat/[id]",
        params: {
          id: target.id,
          kind: target.kind,
          ...(target.name ? { name: target.name } : {}),
        },
      } as Href;
      if (mode.replace) {
        router.replace(href);
      } else {
        router.push(href);
      }
    },
    [regular, groups, router, goToTab],
  );

  const openGroup = useCallback(
    (groupId: string, mode: Mode = {}) => {
      if (regular) {
        if (groupId !== appStore.selectedGroupId) {
          appStore.setSelectedGroupId(groupId);
        }
        goToTab("/(tabs)/groups");
        return;
      }
      const href = { pathname: "/group/[id]", params: { id: groupId } } as Href;
      if (mode.replace) {
        router.replace(href);
      } else {
        router.push(href);
      }
    },
    [regular, router, goToTab],
  );

  return { openConversation, openGroup };
}
