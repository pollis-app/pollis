// Opening a conversation or group from outside its tab. Regular (iPad): select it and `dismissTo` the tab's two-pane.

import { useCallback } from "react";
import { useRouter, useSegments, type Href } from "expo-router";
import { useIsRegular } from "./useLayoutClass";
import { markConversationRead, useUserGroupsWithChannels } from "./queries";
import { appStore } from "../stores/appStore";

export type ConversationTarget = {
  id: string;
  kind: "channel" | "dm";
  name?: string;
  groupId?: string;
};

type Mode = { replace?: boolean };

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
      // The chat screen joins a channel's room by the selected group: select it.
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
