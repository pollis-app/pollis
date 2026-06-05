import React, { useEffect, useLayoutEffect, useRef, useMemo } from "react";
import { MessageItem } from "./MessageItem";
import { useBlockedUsers } from "../../hooks/queries";
import { useGroupMembers } from "../../hooks/queries/useGroups";
import { observer } from "mobx-react-lite";
import { rosterChangeStore, type RosterBanner } from "../../stores/rosterChangeStore";
import type { Message } from "../../types";

const toMs = (timestamp: number): number =>
  timestamp < 1e12 ? timestamp * 1000 : timestamp;

const startOfLocalDay = (d: Date): number =>
  new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

const formatDayDividerLabel = (timestamp: number): string => {
  const d = new Date(toMs(timestamp));
  const now = new Date();
  const dayStart = startOfLocalDay(d);
  const todayStart = startOfLocalDay(now);
  const dayDiff = Math.round((todayStart - dayStart) / 86_400_000);

  if (dayDiff === 0) {
    return "Today";
  }
  if (dayDiff === 1) {
    return "Yesterday";
  }
  if (dayDiff > 1 && dayDiff <= 6) {
    return d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
  }
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  }
  return d.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
};

const DayDivider: React.FC<{ label: string }> = ({ label }) => (
  <div
    data-testid="day-divider"
    className="flex items-center gap-3 py-2 select-none"
  >
    <div className="flex-1 h-px" style={{ background: "var(--c-border)" }} />
    <span
      className="text-xs font-mono"
      style={{ color: "var(--c-text-muted)" }}
    >
      {label}
    </span>
    <div className="flex-1 h-px" style={{ background: "var(--c-border)" }} />
  </div>
);

const RosterChangeBanner: React.FC<{
  banner: RosterBanner;
  resolveName: (userId: string) => string;
}> = ({ banner, resolveName }) => {
  const name = resolveName(banner.payload.user_id);
  let label: string;
  switch (banner.payload.kind) {
    case "joined":
      label = `${name} joined the group`;
      break;
    case "left":
      label = `${name} left the group`;
      break;
    case "device_added":
      label = `${name} added a new device`;
      break;
    case "device_removed":
      label = `${name} removed a device`;
      break;
  }
  return (
    <div
      data-testid={`roster-banner-${banner.id}`}
      className="flex items-center gap-3 py-2 select-none"
    >
      <div className="flex-1 h-px" style={{ background: "var(--c-border)" }} />
      <span
        className="text-xs font-mono"
        style={{ color: "var(--c-text-muted)" }}
      >
        {label}
      </span>
      <div className="flex-1 h-px" style={{ background: "var(--c-border)" }} />
    </div>
  );
};

interface MessageListProps {
  messages: Message[];
  /** MLS group / DM conversation id. When set, inline roster-change
   *  banners (X joined / X added a device / X left) interleave with
   *  messages chronologically. Omit on surfaces with no roster (search
   *  results, threads). */
  conversationId?: string | null;
  /** Group id for resolving display names in roster banners. Equal to
   *  `conversationId` for top-level group MLS; null for DMs. */
  groupIdForNames?: string | null;
  adminUserIds?: Set<string>;
  /** True when the viewer is an admin in this list's group — enables
   * deleting other members' messages for moderation. */
  viewerIsAdmin?: boolean;
  onReply?: (messageId: string) => void;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  onPin?: (messageId: string) => void;
  onScrollToMessage?: (messageId: string) => void;
  getAuthorUsername?: (authorId: string, message?: Message) => string;
  hasMore?: boolean;
  isFetchingMore?: boolean;
  onLoadMore?: () => void;
}

export const MessageList: React.FC<MessageListProps> = observer(({
  messages,
  conversationId,
  groupIdForNames,
  adminUserIds,
  viewerIsAdmin = false,
  onReply,
  onEdit,
  onDelete,
  onScrollToMessage,
  getAuthorUsername,
  hasMore,
  isFetchingMore,
  onLoadMore,
}) => {
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const prevLengthRef = useRef(0);
  const { data: blockedUsers = [] } = useBlockedUsers();
  const blockedIds = useMemo(
    () => new Set(blockedUsers.map((b) => b.user_id)),
    [blockedUsers],
  );
  // Saved scroll metrics taken just before a load-more fetch begins, used to
  // restore relative scroll position after older messages are prepended.
  const savedScrollRef = useRef<{ scrollTop: number; scrollHeight: number } | null>(null);

  const sortedMessages = useMemo(
    () =>
      [...messages]
        // Filter out blank messages — but keep any message with attachments even
        // if the text caption is empty.
        .filter((m) => {
          const hasText = m.content_decrypted != null && m.content_decrypted !== "";
          const hasAttachments = (m.attachments?.length ?? 0) > 0;
          // content_decrypted === undefined means decryption failed — keep the
          // message so it renders as [encrypted] instead of vanishing silently.
          const decryptionFailed = m.content_decrypted === undefined;
          // Soft-deleted messages have no content but should remain visible as "[deleted]".
          const isSoftDeleted = !!m.deleted_at;
          return hasText || hasAttachments || decryptionFailed || isSoftDeleted;
        })
        .sort((a, b) => a.created_at - b.created_at),
    [messages]
  );

  // Roster-change banners pulled from the per-conversation store. Used
  // for display-name resolution: groupIdForNames === conversationId for
  // group MLS, so this read is normally cached by the same query the
  // member list page uses.
  const rosterBanners =
    (conversationId ? rosterChangeStore.byConversation[conversationId] : undefined) ?? [];
  const { data: groupMembers = [] } = useGroupMembers(groupIdForNames ?? null);
  const usernameByUserId = useMemo(() => {
    const map = new Map<string, string>();
    for (const m of groupMembers) {
      map.set(m.user_id, m.username ?? m.user_id);
    }
    return map;
  }, [groupMembers]);
  const resolveName = (userId: string) => usernameByUserId.get(userId) ?? userId;

  // Interleave messages + roster banners by timestamp. Messages use
  // `created_at` (Rust unix seconds or ms — `toMs` normalizes); banners
  // carry an `observed_at_ms` set when the realtime event landed.
  type TimelineItem =
    | { kind: "message"; key: string; ts: number; message: Message }
    | { kind: "banner"; key: string; ts: number; banner: RosterBanner };
  const timeline: TimelineItem[] = useMemo(() => {
    const items: TimelineItem[] = sortedMessages.map((message) => ({
      kind: "message",
      key: `msg:${message.id}`,
      ts: toMs(message.created_at),
      message,
    }));
    for (const banner of rosterBanners) {
      items.push({
        kind: "banner",
        key: `banner:${banner.id}`,
        ts: banner.observed_at_ms,
        banner,
      });
    }
    items.sort((a, b) => a.ts - b.ts);
    return items;
  }, [sortedMessages, rosterBanners]);

  // Scroll to bottom when new messages arrive (but not when older pages load).
  useEffect(() => {
    if (sortedMessages.length > prevLengthRef.current && !isFetchingMore) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    prevLengthRef.current = sortedMessages.length;
  }, [sortedMessages.length, isFetchingMore]);

  // When a load-more fetch starts, save current scroll metrics so position can
  // be restored after older messages are prepended.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    if (isFetchingMore) {
      savedScrollRef.current = {
        scrollTop: container.scrollTop,
        scrollHeight: container.scrollHeight,
      };
    }
  }, [isFetchingMore]);

  // After the message list grows following a load-more, restore relative scroll
  // so the previously visible messages stay in view.
  useLayoutEffect(() => {
    const container = containerRef.current;
    const saved = savedScrollRef.current;
    if (!container || !saved || isFetchingMore) {
      return;
    }
    const heightDelta = container.scrollHeight - saved.scrollHeight;
    if (heightDelta > 0) {
      container.scrollTop = saved.scrollTop + heightDelta;
      savedScrollRef.current = null;
    }
  }, [sortedMessages.length, isFetchingMore]);

  // Trigger load-more when the user scrolls near the top.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    const handleScroll = () => {
      if (container.scrollTop < 150 && hasMore && !isFetchingMore) {
        onLoadMore?.();
      }
    };
    container.addEventListener("scroll", handleScroll, { passive: true });
    return () => container.removeEventListener("scroll", handleScroll);
  }, [hasMore, isFetchingMore, onLoadMore]);

  const scrollToMessage = (messageId: string) => {
    const el = containerRef.current?.querySelector(`[data-testid="message-${messageId}"]`);
    if (el) {
      el.scrollIntoView({ behavior: "smooth", block: "center" });
    }
    onScrollToMessage?.(messageId);
  };

  if (timeline.length === 0) {
    return (
      <div
        data-testid="empty-messages"
        className="flex-1 flex items-center justify-center"
        style={{ background: "var(--c-bg)" }}
      >
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          No messages yet. Start the conversation.
        </p>
      </div>
    );
  }

  return (
    <div
      data-testid="message-list"
      ref={containerRef}
      className="flex-1 overflow-y-auto min-h-0"
      style={{ background: "var(--c-bg)" }}
    >
      {isFetchingMore && (
        <p
          className="text-xs font-mono text-center py-2"
          style={{ color: "var(--c-text-muted)" }}
        >
          Loading…
        </p>
      )}
      {timeline.map((item, idx) => {
        // Day divider only fires on message items (banners aren't tied to
        // a wall-clock day in the same way — they're notifications).
        const prev = idx > 0 ? timeline[idx - 1] : null;
        const currentDay = startOfLocalDay(new Date(item.ts));
        const prevDay = prev ? startOfLocalDay(new Date(prev.ts)) : null;
        const showDivider =
          item.kind === "message" && (prevDay === null || prevDay !== currentDay);

        if (item.kind === "banner") {
          return (
            <RosterChangeBanner
              key={item.key}
              banner={item.banner}
              resolveName={resolveName}
            />
          );
        }

        const { message } = item;
        const rendered = blockedIds.has(message.sender_id) ? (
          <div
            key={message.id}
            data-testid={`message-blocked-${message.id}`}
            className="px-4 py-1"
          >
            <div className="flex items-start gap-2 min-w-0">
              <span
                className="flex-shrink-0 font-mono text-sm"
                style={{ color: "var(--c-text-dim)" }}
              >
                blocked user
              </span>
              <span
                className="font-mono text-sm"
                style={{ color: "var(--c-text-muted)" }}
              >
                [blocked]
              </span>
            </div>
          </div>
        ) : (
          <MessageItem
            key={message.id}
            message={message}
            allMessages={sortedMessages}
            authorUsername={
              getAuthorUsername
                ? getAuthorUsername(message.sender_id, message)
                : "Unknown"
            }
            isAuthorAdmin={adminUserIds?.has(message.sender_id) ?? false}
            canModerate={viewerIsAdmin}
            onReply={onReply}
            onEdit={onEdit}
            onDelete={onDelete}
            onScrollToReply={scrollToMessage}
          />
        );

        return (
          <React.Fragment key={item.key}>
            {showDivider && (
              <DayDivider label={formatDayDividerLabel(message.created_at)} />
            )}
            {rendered}
          </React.Fragment>
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
});
