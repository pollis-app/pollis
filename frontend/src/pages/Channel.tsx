import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Pencil, Trash2 } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useMarkConversationRead } from "../hooks/queries/useUnread";

export const ChannelPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/channels/$channelId" });
  const setSelectedChannelId = appStore.setSelectedChannelId;
  const setSelectedGroupId = appStore.setSelectedGroupId;
  const markConversationRead = useMarkConversationRead(appStore.currentUser?.id ?? null);
  const pendingDeleteChannelId = appStore.pendingDeleteChannelId;
  const setPendingDeleteChannelId = appStore.setPendingDeleteChannelId;

  useEffect(() => {
    // selectedGroupId drives the LiveKit room id used by the typing
    // publisher; without it, this client's typing events never go out.
    // setSelectedGroupId also nulls channelId, so set the group first.
    if (appStore.selectedGroupId !== groupId) {
      setSelectedGroupId(groupId);
    }
    setSelectedChannelId(channelId);
    // Clear the unread badge regardless of how the user got here — direct
    // URL, breadcrumb back-nav, deep link. Previously markRead only fired
    // from the Group page's channel-list click handler, so a direct route
    // visit left the bottom-bar count stuck.
    // Clears the badge optimistically AND advances the persisted cursor, so
    // the channel is still read after a restart (#844).
    markConversationRead.mutate(channelId);
    return () => { setSelectedChannelId(null); };
  }, [groupId, channelId, setSelectedGroupId, setSelectedChannelId, markConversationRead]);

  useEffect(() => {
    return () => { setPendingDeleteChannelId(null); };
  }, [channelId, setPendingDeleteChannelId]);

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const isAdmin = group?.current_user_role === "admin";

  const title = channel ? channel.name : t("channel.fallbackTitle");

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 flex-shrink-0 text-xs font-mono h-bar border-b border-line text-muted"
      >
        <span className="flex-1">{title}</span>
        {isAdmin && channel && pendingDeleteChannelId !== channelId && (
          <div className="flex items-center gap-2">
            <button
              data-testid="rename-channel-trigger"
              onClick={() => navigate({ to: "/groups/$groupId/channels/$channelId/rename", params: { groupId, channelId } })}
              aria-label={t("channel.renameLabel")}
              className="icon-btn-sm flex-shrink-0 padding-0"
            >
              <Pencil size={14} aria-hidden="true" />
            </button>
            <button
              data-testid="delete-channel-trigger"
              onClick={() => setPendingDeleteChannelId(channelId)}
              aria-label={t("channel.deleteLabel")}
              className="icon-btn-sm flex-shrink-0 padding-0"
            >
              <Trash2 size={14} aria-hidden="true" />
            </button>
          </div>
        )}
      </div>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
});
