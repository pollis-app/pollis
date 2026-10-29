import { useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";
import { writeClipboardText } from "../bridge";
import { useUserGroupsWithChannels } from "./queries/useGroups";
import { useResolvePermalink } from "./queries/useBookmarks";
import { messageJumpStore } from "../stores/messageJumpStore";
import { appStore } from "../stores/appStore";
import {
  formatMessagePermalink,
  routeForConversation,
} from "../utils/urlRouting";

/** Copy and open permalinks (#854), resolved only against the local DB. */
export function useMessagePermalink() {
  const navigate = useNavigate();
  const { data: groups = [] } = useUserGroupsWithChannels();
  const resolve = useResolvePermalink();

  /** Copy a permalink; false if the clipboard write failed. */
  const copyPermalink = useCallback(
    async (conversationId: string, messageId: string): Promise<boolean> => {
      return writeClipboardText(
        formatMessagePermalink(conversationId, messageId),
      );
    },
    [],
  );

  /** Navigate to a message and flash it; false (no navigation) when this device lacks it. */
  const openPermalink = useCallback(
    async (conversationId: string, messageId: string): Promise<boolean> => {
      const target = await resolve(conversationId, messageId);
      if (!target.found) {
        return false;
      }

      messageJumpStore.request(conversationId, messageId);

      const route = routeForConversation(conversationId, groups);
      if (route.kind === "channel") {
        appStore.setSelectedGroupId(route.groupId);
        appStore.setSelectedChannelId(route.channelId);
        navigate({
          to: "/groups/$groupId/channels/$channelId",
          params: { groupId: route.groupId, channelId: route.channelId },
        });
      } else {
        appStore.setSelectedConversationId(route.conversationId);
        navigate({
          to: "/dms/$conversationId",
          params: { conversationId: route.conversationId },
        });
      }
      return true;
    },
    [resolve, groups, navigate],
  );

  return { copyPermalink, openPermalink };
}
