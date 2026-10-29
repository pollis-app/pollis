import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { SearchView } from "../components/Search/SearchView";
import { appStore } from "../stores/appStore";
import { messageJumpStore } from "../stores/messageJumpStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { routeForConversation } from "../utils/urlRouting";
import type { SearchResult } from "../types";
import { observer } from "mobx-react-lite";

export const SearchPage: React.FC = observer(() => {
  const { t } = useTranslation("search");
  const navigate = useNavigate();
  const { q } = useSearch({ strict: false }) as { q?: string };
  const { data: groups = [] } = useUserGroupsWithChannels();

  /**
   * Open a hit where it lives: `conversation_kind` / `group_id` from the
   * result, the local group tree as fallback. The jump is queued first;
   * `MessageList` claims it once the row exists.
   */
  const openResult = (result: SearchResult) => {
    messageJumpStore.request(result.conversation_id, result.message_id);

    if (result.conversation_kind === "channel" && result.group_id) {
      appStore.setSelectedGroupId(result.group_id);
      appStore.setSelectedChannelId(result.conversation_id);
      navigate({
        to: "/groups/$groupId/channels/$channelId",
        params: { groupId: result.group_id, channelId: result.conversation_id },
        search: { message: result.message_id },
      });
      return;
    }

    const route = routeForConversation(result.conversation_id, groups);
    if (route.kind === "channel") {
      appStore.setSelectedGroupId(route.groupId);
      appStore.setSelectedChannelId(route.channelId);
      navigate({
        to: "/groups/$groupId/channels/$channelId",
        params: { groupId: route.groupId, channelId: route.channelId },
        search: { message: result.message_id },
      });
      return;
    }

    appStore.setSelectedConversationId(route.conversationId);
    navigate({
      to: "/dms/$conversationId",
      params: { conversationId: route.conversationId },
      search: { message: result.message_id },
    });
  };

  return (
    <PageShell title={t("page.title")}>
      <SearchView
        onOpenResult={openResult}
        onOpenRetentionSettings={() => navigate({ to: "/preferences" })}
        initialQuery={q ?? ""}
      />
    </PageShell>
  );
});
