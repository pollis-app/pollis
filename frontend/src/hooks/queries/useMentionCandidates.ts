import { useMemo } from "react";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useGroupMembers } from "./useGroups";
import { useDMConversations } from "./useMessages";
import type { MentionCandidate } from "../../utils/mentions";

/** Mentionable people (#843), only from membership the user can already see; never a directory lookup. Excludes self. */
export function useMentionCandidates(): MentionCandidate[] {
  const currentUser = useObserver(() => appStore.currentUser);
  const selectedGroupId = useObserver(() => appStore.selectedGroupId);
  const selectedChannelId = useObserver(() => appStore.selectedChannelId);
  const selectedConversationId = useObserver(() => appStore.selectedConversationId);

  const groupMembers = useGroupMembers(selectedChannelId ? selectedGroupId : null);
  const dmConversations = useDMConversations();

  const members = groupMembers.data;
  const conversations = dmConversations.data;

  return useMemo(() => {
    if (selectedChannelId) {
      return (members ?? [])
        .filter((m) => m.user_id !== currentUser?.id && !!m.username)
        .map((m) => ({
          userId: m.user_id,
          username: m.username!,
          displayName: m.display_name,
          avatarUrl: m.avatar_url,
        }));
    }

    if (selectedConversationId) {
      const dm = (conversations ?? []).find((c) => c.id === selectedConversationId);
      if (!dm?.user2_id) {
        return [];
      }
      return [
        {
          userId: dm.user2_id,
          username: dm.user2_identifier,
          avatarUrl: dm.user2_avatar_url,
        },
      ];
    }

    return [];
  }, [selectedChannelId, selectedConversationId, members, conversations, currentUser?.id]);
}
