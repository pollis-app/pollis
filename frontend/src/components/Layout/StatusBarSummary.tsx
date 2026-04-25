import React, { useMemo } from "react";
import { useRouter } from "@tanstack/react-router";
import { Hash, MessageCircle, UserPlus, Mail } from "lucide-react";
import { useAppStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useAllPendingJoinRequests, usePendingInvites } from "../../hooks/queries/useGroups";

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
      className="flex items-center gap-1 font-mono text-xs transition-colors"
      style={{
        background: "none",
        border: "none",
        padding: 0,
        color,
        cursor: "pointer",
      }}
      onMouseEnter={(e) => {
        (e.currentTarget as HTMLButtonElement).style.opacity = "0.7";
      }}
      onMouseLeave={(e) => {
        (e.currentTarget as HTMLButtonElement).style.opacity = "1";
      }}
    >
      {icon}
      <span
        style={{
          display: "inline-block",
          minWidth: "2ch",
          textAlign: "left",
          fontVariantNumeric: "tabular-nums",
          lineHeight: "1.5rem"
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
export const StatusBarSummary: React.FC<StatusBarSummaryProps> = ({ color }) => {
  const unreadCounts = useAppStore((s) => s.unreadCounts);
  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingJoinRequests = [] } = useAllPendingJoinRequests();
  const { data: pendingInvites = [] } = usePendingInvites();

  const groupUnread = useMemo(() => {
    let sum = 0;
    for (const g of groupsWithChannels) {
      for (const ch of g.channels) {
        sum += unreadCounts[ch.id] ?? 0;
      }
    }
    return sum;
  }, [groupsWithChannels, unreadCounts]);

  const dmUnread = useMemo(() => {
    let sum = 0;
    for (const c of dmConversations) {
      sum += unreadCounts[c.id] ?? 0;
    }
    return sum;
  }, [dmConversations, unreadCounts]);

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
};
