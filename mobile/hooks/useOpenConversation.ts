// Opening a conversation or a group from anywhere outside its tab (Search,
// Saved, a profile's "Message", New message, a notification, a permalink, an
// invite, a just-created group).
//
// Compact (phones): exactly the old push/replace of `chat/[id]` /
// `group/[id]`.
// Regular (iPad): the conversation belongs in its tab's two-pane — list on
// the left, conversation on the right, tab bar visible — so select it in the
// store and return to the tab (`dismissTo` pops whatever was pushed over the
// tabs, or replaces when nothing was, e.g. a cold deep link). The tab screen
// renders the selection in its detail pane.

import { useCallback } from "react";
import { useRouter, useSegments, type Href } from "expo-router";
import { useIsRegular } from "./useLayoutClass";
import { useUserGroupsWithChannels } from "./queries";
import { appStore } from "../stores/appStore";

export type ConversationTarget = {
  id: string;
  kind: "channel" | "dm";
  name?: string;
  // The channel's group, when the caller knows it; looked up otherwise.
  groupId?: string;
};

type Mode = { replace?: boolean };

// Store side of "open this conversation in its tab". Returns the tab route.
export function selectConversation(
  target: ConversationTarget,
  groupIdForChannel: string | null,
): "/(tabs)/groups" | "/(tabs)/direct" {
  if (target.kind === "dm") {
    appStore.setSelectedConversationId(target.id);
    appStore.markRead(target.id);
    return "/(tabs)/direct";
  }
  if (groupIdForChannel && groupIdForChannel !== appStore.selectedGroupId) {
    appStore.setSelectedGroupId(groupIdForChannel);
  }
  appStore.setSelectedChannelId(target.id);
  appStore.markRead(target.id);
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
