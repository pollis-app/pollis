import { useMemo } from "react";
import { useGroupMembers, useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";
import { useMentionCandidates } from "../../hooks/queries/useMentionCandidates";
import { useSkin } from "../../hooks/queries/usePreferences";
import { appStore } from "../../stores/appStore";
import { botHandleSet, botIdSet } from "../../utils/bots";
import { mentionLabels } from "../../utils/groupAlias";
import { useBackgroundIsLight } from "../../utils/usernameColor";
import type { MessageRenderContext } from "./messageRenderContext";

/**
 * The render inputs a message body needs (#874), for any view that draws one: the message list
 * and the pinned list (#2127). Read inside an `observer()`; the result is memoised.
 */
export function useMessageRenderContext(
  groupIdForNames: string | null,
  linkPreviews: boolean,
): MessageRenderContext {
  const skin = useSkin();
  const isLightBg = useBackgroundIsLight();
  const currentUser = appStore.currentUser;
  const { data: groupMembers = [] } = useGroupMembers(groupIdForNames);
  // Per-group aliases (#2003): only inside a group; a DM shows main profiles.
  const aliases = useGroupAliases(groupIdForNames);
  const aliasMentionLabels = useMemo(
    () => mentionLabels(aliases, groupMembers),
    [aliases, groupMembers],
  );
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const groupName = groupIdForNames
    ? groupsWithChannels?.find((g) => g.id === groupIdForNames)?.name
    : undefined;
  // Bot badges (#1182) on a bot's messages and on mentions of it.
  const botIds = useMemo(() => botIdSet(groupMembers), [groupMembers]);
  const botHandles = useMemo(() => botHandleSet(groupMembers), [groupMembers]);

  const mentionCandidates = useMentionCandidates();
  const selfName = currentUser?.username?.toLowerCase();
  const mentionNames = useMemo(() => {
    const set = new Set<string>(["all"]);
    for (const c of mentionCandidates) {
      set.add(c.username.toLowerCase());
    }
    if (selfName) {
      set.add(selfName);
    }
    return set;
  }, [mentionCandidates, selfName]);
  // A quoted original from before this user joined never reaches them (`utils/replyQuote.ts`).
  const viewerJoinedAtMs = useMemo(() => {
    const me = groupMembers.find((m) => m.user_id === currentUser?.id);
    const ms = me ? Date.parse(me.joined_at) : NaN;
    return Number.isFinite(ms) ? ms : null;
  }, [groupMembers, currentUser?.id]);

  return useMemo<MessageRenderContext>(
    () => ({
      skin,
      isLightBg,
      currentUserId: currentUser?.id,
      mentionNames,
      selfName,
      viewerJoinedAtMs,
      linkPreviews,
      aliases,
      mentionLabels: aliasMentionLabels,
      botIds,
      botHandles,
      groupName,
    }),
    [
      skin,
      isLightBg,
      currentUser?.id,
      mentionNames,
      selfName,
      viewerJoinedAtMs,
      linkPreviews,
      aliases,
      aliasMentionLabels,
      botIds,
      botHandles,
      groupName,
    ],
  );
}
