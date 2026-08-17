// Conversation id → chat-route params (mobile analogue of desktop's
// `routeForConversation` in utils/urlRouting.ts): an id that matches a known
// channel routes as that channel (selecting its group so realtime joins the
// right room); anything else routes as a DM. Used by the Saved screen and
// the pollis://m/… deep link after a successful permalink resolve.

import { useCallback } from "react";
import { useUserGroupsWithChannels, useDMChannels } from "./queries";
import type { ConversationKind } from "./queries";
import { appStore } from "../stores/appStore";

export interface ConversationRoute {
  kind: ConversationKind;
  name?: string;
}

export function useConversationRoute() {
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dms = [] } = useDMChannels();

  return useCallback(
    (conversationId: string): ConversationRoute => {
      for (const group of groups) {
        const channel = group.channels.find((c) => c.id === conversationId);
        if (channel) {
          appStore.setSelectedGroupId(group.id);
          return { kind: "channel", name: channel.name };
        }
      }
      const dm = dms.find((d) => d.id === conversationId);
      if (dm) {
        return { kind: "dm", name: dm.user2_identifier };
      }
      // Not in any local list — a DM the list hasn't refreshed for. The
      // permalink already resolved locally, so the chat screen can load it.
      return { kind: "dm" };
    },
    [groups, dms],
  );
}
