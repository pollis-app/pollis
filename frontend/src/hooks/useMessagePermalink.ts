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
 * Copying and opening message permalinks (#854).
 *
 * A permalink is `pollis://m/<conversation_id>/<message_id>` — a pointer, not a
 * capability. Opening one is resolved strictly against the local database.
 */
export function useMessagePermalink() {
  const navigate = useNavigate();
  const { data: groups = [] } = useUserGroupsWithChannels();
  const resolve = useResolvePermalink();

  /** Put a message's permalink on the clipboard. Returns false if the OS
   *  clipboard write failed. */
  const copyPermalink = useCallback(
    async (conversationId: string, messageId: string): Promise<boolean> => {
      return writeClipboardText(
        formatMessagePermalink(conversationId, messageId),
      );
    },
    [],
  );

  /**
   * Navigate to a message and flash it.
   *
   * Returns false when this device does not hold the message, which is the
   * caller's cue to render the honest empty state. Nothing is navigated in that
   * case — landing the user in a conversation they may not even be in would
   * itself hint that the target exists.
   *
   * Routing resolves the conversation id to a CHANNEL route or a DM route by
   * looking it up locally. It deliberately does not assume DM the way
   * `pages/Search.tsx` does.
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
