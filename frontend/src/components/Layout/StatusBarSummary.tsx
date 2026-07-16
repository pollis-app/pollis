import React, { useEffect, useMemo } from "react";
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

// Reserve space for two digits whether or not the count is rendered, so the
// row doesn't reflow as unread counts tick in and out.
const SummaryItem: React.FC<SummaryItemProps> = ({ icon, count, to, label, color, testId }) => {
  const router = useRouter();
  // Cap visible count at 99 — we only have space for two digits.
  const display = count <= 0 ? "" : count > 99 ? "99" : String(count);

  return (
    <button
      data-testid={testId}
      aria-label={`${label}: ${count}`}
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
          textAlign: "left",
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

/**
 * At-a-glance unread counts rendered on the left side of the bottom bar:
 * unread channel messages across groups, unread DMs, and pending group
 * join requests you need to act on. Zeros render as blank space so the
 * row doesn't jitter as counts change.
 */
export const StatusBarSummary: React.FC<StatusBarSummaryProps> = observer(({ color }) => {
  const unreadCounts = appStore.unreadCounts;
  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingJoinRequests = [] } = useAllPendingJoinRequests();
  const { data: pendingInvites = [] } = usePendingInvites();
  const { data: dmRequests = [] } = useDMRequests();

  // ── Reconcile pending DM-requests / group-invites into the status-bar alert ──
  // The bottom-bar alert (AppShell) is otherwise purely event-driven — it's only
  // ever set from notify.ts when a `dm_created` / `membership_changed` realtime
  // event lands. Anything that arrived while offline, or before connect_rooms
  // subscribed the inbox, shows up in the badges above (these queries refetch on
  // window focus / reconnect and load at cold launch) but never lights the alert.
  // Mirroring the query result here seeds the alert for whatever the live event
  // stream missed, with the real inviter / requester name (issue #396).
  //
  // The effect re-runs whenever the pending data changes — cold launch (undefined
  // → loaded), a focus/reconnect refetch that surfaces a new item — so no extra
  // focus/reconnect plumbing is needed. It only seeds when no alert is already
  // showing, so a fresher event-driven alert is never clobbered and an alert the
  // user dismissed isn't re-raised until the next refetch brings genuinely new
  // pending data.
  useEffect(() => {
    if (appStore.statusBarAlert) {
      return;
    }
    const request = dmRequests[0];
    if (request) {
      const requester = request.members.find((m) => m.user_id === request.created_by);
      appStore.setStatusBarAlert({
        senderUsername: requester?.username ?? "Someone",
        roomId: request.id,
      });
      return;
    }
    const invite = pendingInvites[0];
    if (invite) {
      appStore.setStatusBarAlert({
        senderUsername: invite.inviter_username ?? invite.group_name ?? "Someone",
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
        label="Unread group messages"
        color={color}
      />
      <SummaryItem
        testId="status-bar-dms-unread"
        icon={<MessageCircle size={12} />}
        count={dmUnread}
        to="/dms"
        label="Unread direct messages"
        color={color}
      />
      <SummaryItem
        testId="status-bar-join-requests"
        icon={<UserPlus size={12} />}
        count={joinRequestCount}
        to="/join-requests"
        label="Pending join requests"
        color={color}
      />
      <SummaryItem
        testId="status-bar-invites"
        icon={<Mail size={12} />}
        count={inviteCount}
        to="/invites"
        label="Pending invites"
        color={color}
      />
    </div>
  );
});
