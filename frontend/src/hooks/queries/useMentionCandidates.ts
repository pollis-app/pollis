import { useMemo } from "react";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useGroupMembers } from "./useGroups";
import { useGroupAliases } from "./useGroupAliases";
import { aliasAvatar, aliasName } from "../../utils/groupAlias";
import { useDMConversations } from "./useMessages";
import type { MentionCandidate } from "../../utils/mentions";
import { isBotUser } from "../../utils/bots";

/** Mentionable people (#843), only from membership the user can already see; never a directory lookup. Excludes self. */
export function useMentionCandidates(): MentionCandidate[] {
  const currentUser = useObserver(() => appStore.currentUser);
  const chatGroupId = useObserver(() => appStore.chatGroupId);
  const selectedChannelId = useObserver(() => appStore.selectedChannelId);
  const selectedConversationId = useObserver(() => appStore.selectedConversationId);

  const groupMembers = useGroupMembers(chatGroupId);
  // The picker shows each member as this group does (#2003); it still inserts `@handle`.
  const aliases = useGroupAliases(chatGroupId);
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
          displayName: aliasName(aliases, m.user_id, m.display_name) ?? undefined,
          avatarUrl: aliasAvatar(aliases, m.user_id, m.avatar_url) ?? undefined,
          isBot: isBotUser(m),
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
  }, [selectedChannelId, selectedConversationId, members, conversations, currentUser?.id, aliases]);
}
