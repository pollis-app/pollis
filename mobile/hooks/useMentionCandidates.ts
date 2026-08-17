// Who can be mentioned in the open conversation (#886). Mirrors desktop's
// useMentionCandidates: the pool is ONLY the visible roster — the group's
// member list for channels, the single peer for DMs. There is deliberately
// no directory/user enumeration. The current user is always excluded (you
// can't mention yourself; self-highlighting is a render concern).

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
