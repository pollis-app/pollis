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

/**
 * Message permalinks (#854): `pollis://m/<conversation_id>/<message_id>`, a
 * pointer, not a capability, resolved strictly against the local database.
 */
export function useMessagePermalink() {
  const navigate = useNavigate();
  const { data: groups = [] } = useUserGroupsWithChannels();
  const resolve = useResolvePermalink();

  /** Copy a message's permalink; false if the clipboard write failed. */
  const copyPermalink = useCallback(
    async (conversationId: string, messageId: string): Promise<boolean> => {
      return writeClipboardText(
        formatMessagePermalink(conversationId, messageId),
      );
    },
    [],
  );

  /**
   * Navigate to a message and flash it. Returns false, navigating nowhere,
   * when this device doesn't hold it: landing in the conversation would hint
   * the target exists. The route kind comes from a local lookup.
   */
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
