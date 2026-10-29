// Who can be mentioned (#886): ONLY the visible roster, never a directory; never yourself.

import { useMemo } from "react";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { useGroupMembers, useDMChannels } from "./queries";
import type { ConversationKind } from "./queries";
import type { MentionCandidate } from "../lib/mentions";

export function useMentionCandidates(
  kind: ConversationKind | null,
  conversationId: string | null,
  groupId: string | null,
): MentionCandidate[] {
  const currentUser = useObserver(() => appStore.currentUser);
  const { data: members = [] } = useGroupMembers(
    kind === "channel" ? groupId : null,
  );
  const { data: dms = [] } = useDMChannels();

  return useMemo(() => {
    if (kind === "channel") {
      return members
        .filter((m) => m.user_id !== currentUser?.id && !!m.username)
        .map((m) => ({
          userId: m.user_id,
          username: m.username!,
          avatarUrl: m.avatar_url,
        }));
    }
    if (kind === "dm" && conversationId) {
      const dm = dms.find((c) => c.id === conversationId);
      if (dm?.user2_id && dm.user2_identifier) {
        return [
          {
            userId: dm.user2_id,
            username: dm.user2_identifier,
            avatarUrl: dm.user2_avatar_url,
          },
        ];
      }
    }
    return [];
  }, [kind, conversationId, members, dms, currentUser?.id]);
}
