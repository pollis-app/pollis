import { useMemo } from "react";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useGroupMembers } from "./useGroups";
import { useDMConversations } from "./useMessages";
import type { MentionCandidate } from "../../utils/mentions";

/**
 * The people the current user may `@mention` in the conversation they're
 * looking at (#843).
 *
 * PRIVACY: the pool is derived exclusively from membership the user can
 * already see — the selected group's roster, or the other party in the
 * selected DM. There is deliberately NO username lookup behind this: a
 * directory search would let anyone enumerate users outside their own
 * groups and DMs just by typing into a composer. The backend resolves
 * mentions against the same roster, so an `@name` nobody in the room
 * answers to is inert on both sides.
 *
 * The current user is excluded — you can't mention yourself.
 */
export function useMentionCandidates(): MentionCandidate[] {
  const currentUser = useObserver(() => appStore.currentUser);
  const selectedGroupId = useObserver(() => appStore.selectedGroupId);
  const selectedChannelId = useObserver(() => appStore.selectedChannelId);
  const selectedConversationId = useObserver(() => appStore.selectedConversationId);

  // Only fetch the roster when a channel is actually selected — a DM composer
  // has no group to read.
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
