import React, { useCallback, useEffect, useLayoutEffect, useRef, useMemo, useState } from "react";
import { MessageItem } from "./MessageItem";
import "./messageHighlight.css";
import { useBlockedUsers } from "../../hooks/queries";
import { useGroupMembers } from "../../hooks/queries/useGroups";
import { observer } from "mobx-react-lite";
import { rosterChangeStore, type RosterBanner } from "../../stores/rosterChangeStore";
import { formatDayDivider } from "../../utils/format";
import { useSkin } from "../../hooks/queries/usePreferences";
import {
  useSavedMessageIds,
  useToggleSavedMessage,
} from "../../hooks/queries/useBookmarks";
import { useMessagePermalink } from "../../hooks/useMessagePermalink";
import { messageJumpStore } from "../../stores/messageJumpStore";
import { useConversationReceipts } from "../../hooks/queries/useReceipts";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { appStore } from "../../stores/appStore";
import type { Message } from "../../types";

// How long the permalink jump highlight stays on the target row.
const HIGHLIGHT_MS = 1800;

const toMs = (timestamp: number): number =>
  timestamp < 1e12 ? timestamp * 1000 : timestamp;

const startOfLocalDay = (d: Date): number =>
  new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

// Time gap (ms) beyond which a same-author message still starts a new group.
const GROUP_GAP_MS = 5 * 60 * 1000;

const DayDivider: React.FC<{ label: string; refined: boolean }> = ({ label, refined }) => {
  if (refined) {
    // Centered mono label between two hairline rules, with density-driven
    // breathing room above and below.
    return (
      <div
        data-testid="day-divider"
        className="flex items-center gap-3 px-4 select-none"
        style={{ marginTop: "var(--msg-divider-gap)", marginBottom: "var(--msg-divider-gap)" }}
      >
        <div className="flex-1 border-t border-line" />
        <span
          className="font-machine text-2xs tabular-nums"
          style={{ color: "var(--c-text-muted)" }}
        >
          {label}
        </span>
        <div className="flex-1 border-t border-line" />
      </div>
    );
  }
  return (
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
};

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
  /** Open a message's thread in the right panel (#825). */
  onOpenThread?: (messageId: string) => void;
  /** messageId -> replies this device holds, for the "N replies" affordance. */
  threadReplyCounts?: Map<string, number>;
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
  onOpenThread,
  threadReplyCounts,
  onEdit,
  onDelete,
  onScrollToMessage,
  getAuthorUsername,
  hasMore,
  isFetchingMore,
  onLoadMore,
}) => {
  const skin = useSkin();
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

  // Delivery / read receipts (#857). Queried ONCE here for the whole list and
  // passed down by message id — a per-row query would be one IPC call per
  // rendered message. Receipts are a DM-only product decision, so a group
  // channel (which always has a selected channel id) never runs this query and
  // never renders an indicator.
  const selectedChannelId = appStore.selectedChannelId;
  const selectedConversationId = appStore.selectedConversationId;
  const isDm = !selectedChannelId && !!selectedConversationId;
  const { receipts } = useConversationReceipts(isDm ? selectedConversationId : null);
  const { data: dmConversations = [] } = useDMConversations();
  // Other members only — the viewer never acknowledges their own message, so a
  // 3-person DM reads "2/2" when both peers have read it, never "2/3".
  const peerCount = useMemo(() => {
    if (!isDm) {
      return 0;
    }
    const conv = dmConversations.find((c) => c.id === selectedConversationId);
    return Math.max((conv?.member_count ?? 2) - 1, 1);
  }, [isDm, dmConversations, selectedConversationId]);
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

  // ── Saved messages + permalinks (#854) ──────────────────────────────────
  const savedMessageIds = useSavedMessageIds();
  const toggleSavedMutation = useToggleSavedMessage();
  const { copyPermalink } = useMessagePermalink();
  // Which message's copy-link button is currently showing an outcome, and
  // which outcome (#889). One at a time: the affordance is per-message but the
  // feedback is about the click that just happened.
  const [copyLinkState, setCopyLinkState] = useState<{
    messageId: string;
    state: "copied" | "failed";
  } | null>(null);

  // Clear the badge a couple of seconds after it appears, and never leave a
  // timer behind that would set state on an unmounted list.
  useEffect(() => {
    if (!copyLinkState) {
      return;
    }
    const timer = window.setTimeout(() => setCopyLinkState(null), 2000);
    return () => window.clearTimeout(timer);
  }, [copyLinkState]);

  const handleToggleSave = useCallback(
    (messageId: string) => {
      toggleSavedMutation.mutate(messageId);
    },
    [toggleSavedMutation],
  );

  const handleCopyLink = useCallback(
    async (messageId: string) => {
      const message = sortedMessages.find((m) => m.id === messageId);
      // The message's OWN conversation, never the list's `conversationId` prop
      // — that prop is the MLS group id for channels, which would produce a
      // permalink that resolves nowhere.
      const targetConversationId = message?.conversation_id;
      if (!targetConversationId) {
        // No conversation to link to is itself a failure the user should see,
        // rather than a click that does nothing at all (#889).
        setCopyLinkState({ messageId, state: "failed" });
        return;
      }
      const ok = await copyPermalink(targetConversationId, messageId);
      setCopyLinkState({ messageId, state: ok ? "copied" : "failed" });
    },
    [sortedMessages, copyPermalink],
  );

  // Read the pending jump during RENDER, not only inside the effect below.
  // `MessageList` is an `observer`, and observers only subscribe to what they
  // touch while rendering — claiming solely inside an effect would mean a jump
  // requested while this conversation is already open never re-renders the
  // component, so the effect would never re-run and the jump would be dropped.
  //
  // The match is on the message this list RENDERS rather than on the
  // `conversationId` prop, which for channels is the MLS *group* id (it feeds
  // roster banners) and so would never equal a channel permalink's target.
  const requestedJumpMessageId = messageJumpStore.messageId;
  const jumpTargetMessageId =
    requestedJumpMessageId &&
    sortedMessages.some((m) => m.id === requestedJumpMessageId)
      ? requestedJumpMessageId
      : null;

  // Consume a pending permalink jump: scroll the target into view and flash it.
  // The class is applied imperatively to the row that is already in the DOM, so
  // this needs no change to `MessageItem` and adds no wrapper to the timeline.
  //
  // A jump requested before its target has loaded simply is not claimed yet —
  // it stays pending and this effect re-runs when the message arrives.
  useEffect(() => {
    if (!jumpTargetMessageId) {
      return;
    }
    const el = containerRef.current?.querySelector(
      `[data-testid="message-${jumpTargetMessageId}"]`,
    );
    if (!el) {
      return;
    }
    if (!messageJumpStore.claimMessage(jumpTargetMessageId)) {
      return;
    }
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    el.classList.add("pollis-message-flash");
    const timer = setTimeout(() => {
      el.classList.remove("pollis-message-flash");
      messageJumpStore.clearHighlight();
    }, HIGHLIGHT_MS);
    return () => clearTimeout(timer);
  }, [jumpTargetMessageId, timeline.length]);

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

        // A message starts a new sender group (refined skin) when it's the
        // first item, when a banner or day-divider separates it from the
        // previous item, when the previous rendered item is a different
        // author, or when the same author's gap exceeds GROUP_GAP_MS.
        const isGroupStart =
          item.kind === "message" &&
          (prev === null ||
            prev.kind === "banner" ||
            showDivider ||
            prev.message.sender_id !== item.message.sender_id ||
            item.ts - prev.ts > GROUP_GAP_MS);

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
            isGroupStart={isGroupStart}
            onReply={onReply}
            onOpenThread={onOpenThread}
            threadReplyCount={threadReplyCounts?.get(message.id) ?? 0}
            onEdit={onEdit}
            onDelete={onDelete}
            onScrollToReply={scrollToMessage}
            isSaved={savedMessageIds.has(message.id)}
            onToggleSave={handleToggleSave}
            onCopyLink={handleCopyLink}
            copyLinkState={
              copyLinkState?.messageId === message.id
                ? copyLinkState.state
                : "idle"
            }
            receipts={receipts}
            peerCount={peerCount}
            isDm={isDm}
          />
        );

        return (
          <React.Fragment key={item.key}>
            {showDivider && (
              <DayDivider
                label={formatDayDivider(toMs(message.created_at))}
                refined={skin === "refined"}
              />
            )}
            {rendered}
          </React.Fragment>
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
});
