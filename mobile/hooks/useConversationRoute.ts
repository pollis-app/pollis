// Conversation id → chat-route params: a known channel routes as one, anything else as a DM.

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
      // Not in any local list: a DM the list has not refreshed for.
      return { kind: "dm" };
    },
    [groups, dms],
  );
}
