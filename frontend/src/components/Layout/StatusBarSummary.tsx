import React, { useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useRouter } from "@tanstack/react-router";
import { Hash, MessageCircle, UserPlus, Mail } from "lucide-react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useAllPendingJoinRequests, usePendingInvites } from "../../hooks/queries/useGroups";
import { useDMRequests } from "../../hooks/queries/useBlocks";

interface SummaryItemProps {
  icon: React.ReactNode;
  count: number;
  to: string;
  label: string;
  color: string;
  testId: string;
}

// Space for two digits always, so the row doesn't reflow.
const SummaryItem: React.FC<SummaryItemProps> = ({ icon, count, to, label, color, testId }) => {
  const router = useRouter();
  const display = count <= 0 ? "" : count > 99 ? "99" : String(count);

  return (
    <button
      data-testid={testId}
      aria-label={label}
      onClick={() => router.navigate({ to })}
      className={`bg-transparent flex items-center gap-1 font-mono text-xs hover:opacity-50`}
      style={{
        color,
      }}
    >
      {icon}
      <span
        style={{
          display: "inline-block",
          minWidth: "2ch",
          height: "1em",
          lineHeight: 1,
          textAlign: "start",
          fontVariantNumeric: "tabular-nums",
        }}
      >
        {display}
      </span>
    </button>
  );
};

interface StatusBarSummaryProps {
  color: string;
}

/** Unread channels, unread DMs and pending join requests; zeros render blank. */
export const StatusBarSummary: React.FC<StatusBarSummaryProps> = observer(({ color }) => {
  const { t } = useTranslation("nav");
  const unreadCounts = appStore.unreadCounts;
  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingJoinRequests = [] } = useAllPendingJoinRequests();
  const { data: pendingInvites = [] } = usePendingInvites();
  const { data: dmRequests = [] } = useDMRequests();

  // Seed the alert from pending requests/invites the realtime stream missed (#396), only when no alert is showing.
  useEffect(() => {
    if (appStore.statusBarAlert) {
      return;
    }
    const request = dmRequests[0];
    if (request) {
      const requester = request.members.find((m) => m.user_id === request.created_by);
      appStore.setStatusBarAlert({
        senderUsername: requester?.username ?? t("statusBar.someone"),
        roomId: request.id,
      });
      return;
    }
    const invite = pendingInvites[0];
    if (invite) {
      appStore.setStatusBarAlert({
        senderUsername: invite.inviter_username ?? invite.group_name ?? t("statusBar.someone"),
        roomId: invite.group_id,
      });
    }
  }, [dmRequests, pendingInvites]);

  const groupUnread = useMemo(
    () => groupsWithChannels.reduce((sum, g) => sum + appStore.unreadFor(g.channels), 0),
    [groupsWithChannels, unreadCounts]
  );

  const dmUnread = useMemo(
    () => appStore.unreadFor(dmConversations),
    [dmConversations, unreadCounts]
  );

  const joinRequestCount = pendingJoinRequests.length;
  const inviteCount = pendingInvites.length;

  return (
    <div data-testid="status-bar-summary" className="flex items-center gap-3">
      <SummaryItem
        testId="status-bar-groups-unread"
        icon={<Hash size={12} />}
        count={groupUnread}
        to="/groups"
        label={t("statusBar.groupsUnread", { count: groupUnread })}
        color={color}
      />
      <SummaryItem
        testId="status-bar-dms-unread"
        icon={<MessageCircle size={12} />}
        count={dmUnread}
        to="/dms"
        label={t("statusBar.dmsUnread", { count: dmUnread })}
        color={color}
      />
      <SummaryItem
        testId="status-bar-join-requests"
        icon={<UserPlus size={12} />}
        count={joinRequestCount}
        to="/join-requests"
        label={t("statusBar.joinRequests", { count: joinRequestCount })}
        color={color}
      />
      <SummaryItem
        testId="status-bar-invites"
        icon={<Mail size={12} />}
        count={inviteCount}
        to="/invites"
        label={t("statusBar.invites", { count: inviteCount })}
        color={color}
      />
    </div>
  );
});
