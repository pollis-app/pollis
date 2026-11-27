// What `MessageBodyInline` needs to draw a body as the timeline does (#2127): which `@mentions`
// resolve, which is the viewer, alias labels (#2003) and bot handles (#1182). Shared by the
// conversation screen, the pinned list and the thread screen (#2143).

import { useMemo } from "react";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../stores/appStore";
import { useGroupMembers } from "./queries";
import type { ConversationKind } from "./queries";
import { useGroupAliases } from "./queries/useGroupAliases";
import { useMentionCandidates } from "./useMentionCandidates";
import { botHandles } from "../lib/bots";
import { mentionLabels } from "../lib/groupAlias";

export interface MessageBodyContext {
  mentionNames: ReadonlySet<string>;
  selfName: string | null;
  mentionLabels: ReadonlyMap<string, string>;
  botMentions: ReadonlySet<string>;
}

export function useMessageBodyContext(
  kind: ConversationKind | null,
  conversationId: string | null,
  groupId: string | null,
): MessageBodyContext {
  const currentUser = useObserver(() => appStore.currentUser);
  const channelGroupId = kind === "channel" ? groupId : null;
  const mentionCandidates = useMentionCandidates(kind, conversationId, groupId);
  const { data: groupMembers } = useGroupMembers(channelGroupId);
  const aliases = useGroupAliases(channelGroupId);

  const selfName = currentUser?.username?.toLowerCase() ?? null;
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
  const labels = useMemo(
    () => mentionLabels(aliases, groupMembers ?? []),
    [aliases, groupMembers],
  );
  const botMentions = useMemo(() => botHandles(groupMembers ?? []), [groupMembers]);

  return useMemo(
    () => ({ mentionNames, selfName, mentionLabels: labels, botMentions }),
    [mentionNames, selfName, labels, botMentions],
  );
}
