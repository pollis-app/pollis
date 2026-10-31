import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Bell, BellOff, Pencil, Pin, Trash2 } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useRightPanel } from "../components/Layout/RightPanel/useRightPanel";
import { useMutedChannels, useSetChannelMuted, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useMarkConversationRead } from "../hooks/queries/useUnread";
import { ExportArchiveButton } from "../components/Security/ExportArchiveButton";
import { PressButton } from "../components/ui/PressButton";
import { ConversationHeader } from "../components/Layout/ConversationHeader";

export const ChannelPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/channels/$channelId" });
  const setSelectedChannelId = appStore.setSelectedChannelId;
  const setSelectedGroupId = appStore.setSelectedGroupId;
  const { mutate: markConversationRead } = useMarkConversationRead(appStore.currentUser?.id ?? null);
  const pendingDeleteChannelId = appStore.pendingDeleteChannelId;
  const setPendingDeleteChannelId = appStore.setPendingDeleteChannelId;
  const { togglePins } = useRightPanel();

  useEffect(() => {
    // The group drives the typing room id, and setting it nulls channelId, so set it first.
    if (appStore.selectedGroupId !== groupId) {
      setSelectedGroupId(groupId);
    }
    setSelectedChannelId(channelId);
    // Clear on any nav path, with a durable cursor (#844).
    markConversationRead(channelId);
    return () => { setSelectedChannelId(null); };
  }, [groupId, channelId, setSelectedGroupId, setSelectedChannelId, markConversationRead]);

  useEffect(() => {
    return () => { setPendingDeleteChannelId(null); };
  }, [channelId, setPendingDeleteChannelId]);

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const isAdmin = group?.current_user_role === "admin";
  const { data: mutedChannels } = useMutedChannels();
  const setChannelMuted = useSetChannelMuted();
  const isMuted = mutedChannels?.has(channelId) ?? false;
  const muteLabel = isMuted ? t("channel.unmuteLabel") : t("channel.muteLabel");

  const title = channel ? channel.name : t("channel.fallbackTitle");

  return (
    <div className="flex flex-col h-full">
      <ConversationHeader title={title}>
        <ExportArchiveButton
          key={channelId}
          conversationId={channelId}
          fileStem={`pollis-${channel?.name ?? channelId}`}
          variant="icon"
          testId="channel-export-trigger"
        />
        {/* Per-channel mute (#1431): silences this channel's pushes and
            notifications on every device of the account, except a direct
            @mention. Optimistic, and disabled while the write is in flight. */}
        <button
          data-testid="channel-mute-toggle"
          data-muted={isMuted ? "true" : "false"}
          onClick={() => setChannelMuted.mutate({ channelId, muted: !isMuted })}
          disabled={setChannelMuted.isPending}
          aria-pressed={isMuted}
          aria-label={muteLabel}
          title={`${muteLabel}. ${t("channel.muteDescription")}`}
          className="icon-btn-sm flex-shrink-0"
        >
          {isMuted ? <BellOff size={14} aria-hidden="true" /> : <Bell size={14} aria-hidden="true" />}
        </button>
        <PressButton
          data-testid="channel-pins-trigger"
          onClick={togglePins}
          aria-label={t("channel.pinsLabel")}
          title={t("channel.pinsLabel")}
          className="icon-btn-sm flex-shrink-0"
        >
          <Pin size={14} aria-hidden="true" />
        </PressButton>
        {isAdmin && channel && pendingDeleteChannelId !== channelId && (
          <>
            <PressButton
              data-testid="rename-channel-trigger"
              onClick={() => navigate({ to: "/groups/$groupId/channels/$channelId/rename", params: { groupId, channelId } })}
              aria-label={t("channel.renameLabel")}
              className="icon-btn-sm flex-shrink-0"
            >
              <Pencil size={14} aria-hidden="true" />
            </PressButton>
            <button
              data-testid="delete-channel-trigger"
              onClick={() => setPendingDeleteChannelId(channelId)}
              aria-label={t("channel.deleteLabel")}
              className="icon-btn-sm flex-shrink-0"
            >
              <Trash2 size={14} aria-hidden="true" />
            </button>
          </>
        )}
      </ConversationHeader>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
});
