import React, { useCallback, useEffect, useRef, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useNavigate } from "@tanstack/react-router";
import { MessageItem } from "./MessageItem";
import "./messageHighlight.css";
import {
  estimateRowPx,
  findRow,
  loadMoreThresholdPx,
  revealRow,
  ROW_OVERSCAN,
  type MessageRowWindow,
} from "./messageWindow";
import { useBlockedUsers } from "../../hooks/queries";
import { useGroupMembers, useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { observer } from "mobx-react-lite";
import { formatDayDivider, toMs } from "../../utils/format";
import { useSkin } from "../../hooks/queries/usePreferences";
import {
  useSavedMessageIds,
  useToggleSavedMessage,
} from "../../hooks/queries/useBookmarks";
import {
  usePinnedMessageIds,
  useTogglePinnedMessage,
} from "../../hooks/queries/usePins";
import { useMessagePermalink } from "../../hooks/useMessagePermalink";
import { messageJumpStore } from "../../stores/messageJumpStore";
import { useConversationReceipts } from "../../hooks/queries/useReceipts";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useMentionCandidates } from "../../hooks/queries/useMentionCandidates";
import { useBackgroundIsLight } from "../../utils/usernameColor";
import { probeRender } from "../../utils/renderProbe";
import type { MessageRenderContext } from "./messageRenderContext";
import { appStore } from "../../stores/appStore";
import { messageNavStore } from "../../stores/messageNavStore";
import type { MessageNavAction } from "../../utils/messageNav";
import { arrowStep, isHorizontalArrow, isRtlElement } from "../../utils/direction";
import type { Message } from "../../types";
import { collapseUndecryptable } from "../../utils/collapseUndecryptable";
import { isNewMemberMessage, newMemberJoins } from "../../utils/newMember";
import { EmptyState } from "../ui/EmptyState";

const HIGHLIGHT_MS = 1800;

const startOfLocalDay = (d: Date): number =>
  new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

const GROUP_GAP_MS = 5 * 60 * 1000;

const DayDivider: React.FC<{ label: string; refined: boolean }> = React.memo(({ label, refined }) => {
  if (refined) {
    return (
      <div
        data-testid="day-divider"
        className="flex items-center gap-3 px-4 select-none mt-msg-divider mb-msg-divider"
      >
        <div className="flex-1 border-t border-line" />
        <span
          className="font-machine text-2xs tabular-nums text-muted"
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
      <div className="flex-1 h-px bg-line" />
      <span
        className="text-xs font-mono text-muted"
      >
        {label}
      </span>
      <div className="flex-1 h-px bg-line" />
    </div>
  );
});
DayDivider.displayName = "DayDivider";

interface MessageListProps {
  messages: Message[];
  /** MLS group / DM conversation id; a change resets keyboard navigation. */
  conversationId?: string | null;
  /** Roster for the "new" member tag (#1506); null for DMs. */
  groupIdForNames?: string | null;
  adminUserIds?: Set<string>;
  /** Viewer is an admin here: may delete others' messages. */
  viewerIsAdmin?: boolean;
  onReply?: (messageId: string) => void;
  onOpenThread?: (messageId: string) => void;
  threadReplyCounts?: Map<string, number>;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  onScrollToMessage?: (messageId: string) => void;
  getAuthorUsername?: (authorId: string, message?: Message) => string;
  hasMore?: boolean;
  isFetchingMore?: boolean;
  onLoadMore?: () => void;
  /** An anchored window (#1039): newer messages exist that aren't the live
   * tail. Near the bottom it asks `onLoadNewer`; arrivals don't pull the view. */
  hasNewer?: boolean;
  isFetchingNewer?: boolean;
  onLoadNewer?: () => void;
  /** Bumped when the parent swaps in the live tail wholesale, so the log
   * re-opens at its newest row. */
  windowEpoch?: number;
  /** Opt-in to arrow-key navigation; only the main composer's list passes it,
   * since one mounted list may lend the nav store its context. */
  focusComposer?: () => void;
  /** Channel or DM id pins key on (#99), NOT the MLS group id. Omit to hide pinning. */
  pinsConversationId?: string | null;
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
  pinsConversationId = null,
  onEdit,
  onDelete,
  onScrollToMessage,
  getAuthorUsername,
  hasMore,
  isFetchingMore,
  onLoadMore,
  hasNewer = false,
  isFetchingNewer = false,
  onLoadNewer,
  windowEpoch = 0,
  focusComposer,
}) => {
  probeRender("MessageList");
  const { t } = useTranslation("chat");
  const skin = useSkin();
  const containerRef = useRef<HTMLDivElement>(null);
  const { data: blockedUsers = [] } = useBlockedUsers();
  const blockedIds = useMemo(
    () => new Set(blockedUsers.map((b) => b.user_id)),
    [blockedUsers],
  );

  const sortedMessages = useMemo(
    () =>
      // A run of lost messages (#1312) collapses into one row.
      collapseUndecryptable([...messages]
        // Drop blank messages, but keep attachment-only ones.
        .filter((m) => {
          const hasText = m.content_decrypted != null && m.content_decrypted !== "";
          const hasAttachments = (m.attachments?.length ?? 0) > 0;
          // Undefined means decryption failed: render [encrypted], not nothing.
          const decryptionFailed = m.content_decrypted === undefined;
          const isSoftDeleted = !!m.deleted_at;
          // A lost message's placeholder (#1312) has no content by definition.
          return hasText || hasAttachments || decryptionFailed || isSoftDeleted || !!m.undecryptable;
        })
        .sort((a, b) => a.created_at - b.created_at)),
    [messages]
  );

  // "New" member tag (#1506): tagged for a week after `joined_at`; the only sign
  // someone just arrived, since channels have no membership lines.
  const { data: groupMembers = [] } = useGroupMembers(groupIdForNames ?? null);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const groupCreatorId = groupIdForNames
    ? groupsWithChannels?.find((g) => g.id === groupIdForNames)?.created_by
    : undefined;
  const newMemberJoinMs = useMemo(
    () => newMemberJoins(groupMembers, groupCreatorId),
    [groupMembers, groupCreatorId],
  );

  // Receipts (#857): one query for the whole list, DM-only.
  const selectedChannelId = appStore.selectedChannelId;
  const selectedConversationId = appStore.selectedConversationId;
  const isDm = !selectedChannelId && !!selectedConversationId;
  const { receipts } = useConversationReceipts(isDm ? selectedConversationId : null);
  const { data: dmConversations = [] } = useDMConversations();
  // Peers only: a 3-person DM reads "2/2" when both have read.
  const peerCount = useMemo(() => {
    if (!isDm) {
      return 0;
    }
    const conv = dmConversations.find((c) => c.id === selectedConversationId);
    return Math.max((conv?.member_count ?? 2) - 1, 1);
  }, [isDm, dmConversations, selectedConversationId]);

  // List-wide render inputs, subscribed once and passed as one memoised object
  // so the row memo works (#874).
  const isLightBg = useBackgroundIsLight();
  const currentUser = appStore.currentUser;
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
  const renderCtx = useMemo<MessageRenderContext>(
    () => ({
      skin,
      isLightBg,
      currentUserId: currentUser?.id,
      mentionNames,
      selfName,
    }),
    [skin, isLightBg, currentUser?.id, mentionNames, selfName],
  );

  // Reply targets resolved in one pass, indexing only replied-to messages (#874).
  const replyTargets = useMemo(() => {
    const wanted = new Set<string>();
    for (const m of sortedMessages) {
      if (m.reply_to_message_id) {
        wanted.add(m.reply_to_message_id);
      }
    }
    if (wanted.size === 0) {
      return new Map<string, Message>();
    }
    const map = new Map<string, Message>();
    for (const m of sortedMessages) {
      if (wanted.has(m.id)) {
        map.set(m.id, m);
      }
    }
    return map;
  }, [sortedMessages]);

  // `created_at` is unix seconds or ms; `toMs` normalizes.
  type TimelineItem = { kind: "message"; key: string; ts: number; message: Message };
  const timeline: TimelineItem[] = useMemo(
    () =>
      sortedMessages.map((message) => ({
        kind: "message",
        key: `msg:${message.id}`,
        ts: toMs(message.created_at),
        message,
      })),
    [sortedMessages],
  );

  // Windowed (#874): rows keyed by timeline key, not index, so a prepend keeps
  // measured heights. See `.codesight/wiki/ui.md` → "The windowed log".
  const timelineRef = useRef(timeline);
  timelineRef.current = timeline;
  const getItemKey = useCallback(
    (index: number) => timelineRef.current[index]?.key ?? index,
    [],
  );
  // Per render, not memoised, so it follows a font-size change.
  const rowEstimatePx = estimateRowPx(skin);
  const rowEstimateRef = useRef(rowEstimatePx);
  rowEstimateRef.current = rowEstimatePx;
  const estimateSize = useCallback(() => rowEstimateRef.current, []);

  const virtualizer = useVirtualizer({
    count: timeline.length,
    getScrollElement: () => containerRef.current,
    estimateSize,
    getItemKey,
    overscan: ROW_OVERSCAN,
    // Anchored at the END: re-pins the row being read across any count change.
    anchorTo: "end",
    // Follow new messages only from the bottom. Off in an anchored window
    // (#1039), and one commit past its last append (`isFetchingNewer`).
    followOnAppend: hasNewer || isFetchingNewer ? false : "smooth",
    // One row of slack rather than the 1px default.
    scrollEndThreshold: rowEstimatePx,
  });

  // Message id → timeline index, for subsystems that must ask before they look
  // (`messageWindow.ts`).
  const indexByMessageId = useMemo(() => {
    const map = new Map<string, number>();
    timeline.forEach((item, index) => {
      if (item.kind === "message") {
        map.set(item.message.id, index);
      }
    });
    return map;
  }, [timeline]);
  const indexByMessageIdRef = useRef(indexByMessageId);
  indexByMessageIdRef.current = indexByMessageId;

  // Ref-backed, so an in-flight reveal reads the live timeline.
  const rowWindow = useMemo<MessageRowWindow>(
    () => ({
      container: () => containerRef.current,
      indexOf: (messageId: string) => indexByMessageIdRef.current.get(messageId),
      virtualizer,
    }),
    [virtualizer],
  );
  // In a ref too, so row callbacks keep their empty deps (#874).
  const rowWindowRef = useRef(rowWindow);
  rowWindowRef.current = rowWindow;

  // Open at the newest message. Re-runs rather than latching: `followOnAppend`
  // only reacts to count changes, and dev double-mount rewinds the virtualizer.
  const logKey = `${selectedChannelId ?? ""}|${selectedConversationId ?? ""}`;
  const hasRows = timeline.length > 0;
  useEffect(() => {
    if (!hasRows) {
      return;
    }
    virtualizer.scrollToIndex(timelineRef.current.length - 1, { align: "end" });
  }, [logKey, windowEpoch, hasRows, virtualizer]);

  // Load more at the top of the log, or when the log doesn't fill its viewport.
  const maybeLoadMore = useCallback(() => {
    const container = containerRef.current;
    if (!container || !hasMore || isFetchingMore) {
      return;
    }
    // Read per call: the threshold is in rem (#934).
    const threshold = loadMoreThresholdPx();
    const scrollable = container.scrollHeight - container.clientHeight > threshold;
    if (!scrollable || container.scrollTop < threshold) {
      onLoadMore?.();
    }
  }, [hasMore, isFetchingMore, onLoadMore]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    container.addEventListener("scroll", maybeLoadMore, { passive: true });
    return () => container.removeEventListener("scroll", maybeLoadMore);
  }, [maybeLoadMore]);

  // …and whenever the contents change: a viewport at offset 0 (or one the page
  // fits in) emits no scroll (#927). `maybeLoadMore`'s at-top check stops the
  // loop, since the prepend re-anchors the reader below it.
  useEffect(() => {
    maybeLoadMore();
  }, [maybeLoadMore, timeline.length]);

  // The mirror for an anchored window (#1039): load newer at the window's
  // bottom. Inert in live mode. Not while a jump is pending, since the viewport
  // is still wherever the previous log left it.
  const pendingJumpMessageId = messageJumpStore.messageId;
  const maybeLoadNewer = useCallback(() => {
    const container = containerRef.current;
    if (!container || !hasNewer || isFetchingNewer || pendingJumpMessageId) {
      return;
    }
    const threshold = loadMoreThresholdPx();
    const scrollable = container.scrollHeight - container.clientHeight > threshold;
    const fromBottom = container.scrollHeight - container.scrollTop - container.clientHeight;
    if (!scrollable || fromBottom < threshold) {
      onLoadNewer?.();
    }
  }, [hasNewer, isFetchingNewer, onLoadNewer, pendingJumpMessageId]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    container.addEventListener("scroll", maybeLoadNewer, { passive: true });
    return () => container.removeEventListener("scroll", maybeLoadNewer);
  }, [maybeLoadNewer]);

  useEffect(() => {
    maybeLoadNewer();
  }, [maybeLoadNewer, timeline.length]);

  // Pinned identity: handed to every row as `onScrollToReply` (#874). The
  // quoted parent is often off-window, so it asks the window for the row first.
  const onScrollToMessageRef = useRef(onScrollToMessage);
  onScrollToMessageRef.current = onScrollToMessage;
  const replyRevealRef = useRef(0);
  const scrollToMessage = useCallback((messageId: string) => {
    const token = ++replyRevealRef.current;
    void revealRow(
      rowWindowRef.current,
      messageId,
      "center",
      () => replyRevealRef.current !== token,
    ).then((el) => {
      if (el && replyRevealRef.current === token) {
        el.scrollIntoView({ behavior: "smooth", block: "center" });
      }
    });
    onScrollToMessageRef.current?.(messageId);
  }, []);

  const savedMessageIds = useSavedMessageIds();
  const toggleSavedMutation = useToggleSavedMessage();
  // One pins query + id-set for the whole list (#99).
  const pinnedMessageIds = usePinnedMessageIds(pinsConversationId);
  const togglePinMutation = useTogglePinnedMessage();
  const { copyPermalink } = useMessagePermalink();
  // The one message whose copy-link shows an outcome (#889).
  const [copyLinkState, setCopyLinkState] = useState<{
    messageId: string;
    state: "copied" | "failed";
  } | null>(null);

  // Clear the badge after a moment; never leave a timer on an unmounted list.
  useEffect(() => {
    if (!copyLinkState) {
      return;
    }
    const timer = window.setTimeout(() => setCopyLinkState(null), 2000);
    return () => window.clearTimeout(timer);
  }, [copyLinkState]);

  // Row callbacks read refs, not deps (#874): `useMutation` results and
  // `sortedMessages` change identity constantly.
  const toggleSavedRef = useRef(toggleSavedMutation);
  toggleSavedRef.current = toggleSavedMutation;
  const handleToggleSave = useCallback((messageId: string) => {
    toggleSavedRef.current.mutate(messageId);
  }, []);

  const togglePinRef = useRef(togglePinMutation);
  togglePinRef.current = togglePinMutation;
  const pinnedIdsRef = useRef(pinnedMessageIds);
  pinnedIdsRef.current = pinnedMessageIds;
  const pinsConversationIdRef = useRef(pinsConversationId);
  pinsConversationIdRef.current = pinsConversationId;
  const handleTogglePin = useCallback((messageId: string) => {
    const conversationId = pinsConversationIdRef.current;
    if (!conversationId) {
      return;
    }
    togglePinRef.current.mutate({
      conversationId,
      messageId,
      isPinned: pinnedIdsRef.current.has(messageId),
    });
  }, []);

  const sortedMessagesRef = useRef(sortedMessages);
  sortedMessagesRef.current = sortedMessages;
  const copyPermalinkRef = useRef(copyPermalink);
  copyPermalinkRef.current = copyPermalink;

  const handleCopyLink = useCallback(
    async (messageId: string) => {
      const message = sortedMessagesRef.current.find((m) => m.id === messageId);
      // The message's OWN conversation: the prop is the MLS group id for
      // channels, which would produce a dead permalink.
      const targetConversationId = message?.conversation_id;
      if (!targetConversationId) {
        // Nothing to link to is a visible failure (#889).
        setCopyLinkState({ messageId, state: "failed" });
        return;
      }
      const ok = await copyPermalinkRef.current(targetConversationId, messageId);
      setCopyLinkState({ messageId, state: ok ? "copied" : "failed" });
    },
    [],
  );

  // Report the sender (#1213), naming the message's own conversation.
  const navigate = useNavigate();
  const handleReport = useCallback(
    (messageId: string) => {
      const message = sortedMessagesRef.current.find((m) => m.id === messageId);
      if (!message) {
        return;
      }
      if (message.conversation_id) {
        navigate({
          to: "/report/$userId/$conversationId/$messageId",
          params: { userId: message.sender_id, conversationId: message.conversation_id, messageId },
        });
      } else {
        navigate({ to: "/report/$userId", params: { userId: message.sender_id } });
      }
    },
    [navigate],
  );

  // Read the pending jump during RENDER: an observer subscribes only to what it
  // reads while rendering. Matched on rendered messages, since `conversationId`
  // is the MLS group id for channels.
  const requestedJumpMessageId = messageJumpStore.messageId;
  const jumpTargetMessageId =
    requestedJumpMessageId &&
    sortedMessages.some((m) => m.id === requestedJumpMessageId)
      ? requestedJumpMessageId
      : null;

  // Consume a pending permalink jump: `revealRow` moves the window to the
  // target, then the row is scrolled to and flashed. Claimed only once the row
  // exists, so an unloaded target stays pending.
  useEffect(() => {
    if (!jumpTargetMessageId) {
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    void revealRow(
      rowWindowRef.current,
      jumpTargetMessageId,
      "center",
      () => cancelled,
    ).then((el) => {
      if (!el || cancelled) {
        return;
      }
      if (!messageJumpStore.claimMessage(jumpTargetMessageId)) {
        return;
      }
      el.scrollIntoView({ behavior: "smooth", block: "center" });
      el.classList.add("pollis-message-flash");
      timer = setTimeout(() => {
        el.classList.remove("pollis-message-flash");
        messageJumpStore.clearHighlight();
      }, HIGHLIGHT_MS);
    });
    return () => {
      cancelled = true;
      if (timer) {
        clearTimeout(timer);
      }
    };
  }, [jumpTargetMessageId, timeline.length]);

  // Arrow-key log navigation: machine in `utils/messageNav.ts`, state in
  // `stores/messageNavStore.ts`. Rows style via `focus-within`, so keys
  // re-render nothing.

  // Blocked-author stubs are skipped, like tombstones.
  const navigableIds = useMemo(
    () => sortedMessages.filter((m) => !blockedIds.has(m.sender_id)).map((m) => m.id),
    [sortedMessages, blockedIds],
  );
  const navigableIdsRef = useRef(navigableIds);
  navigableIdsRef.current = navigableIds;
  const focusComposerRef = useRef(focusComposer);
  focusComposerRef.current = focusComposer;

  useEffect(() => {
    // `focusComposer` is the opt-in.
    if (!focusComposer) {
      return;
    }
    const ctx = {
      orderedIds: () => navigableIdsRef.current,
      // The rendered action bar is the source of truth, so nav never selects a
      // button that isn't on screen; the row is focused, hence rendered.
      actionsFor: (messageId: string) => {
        const row = findRow(rowWindowRef.current, messageId);
        if (!row) {
          return [];
        }
        return Array.from(row.querySelectorAll<HTMLElement>("[data-nav-action]")).map(
          (el) => el.dataset.navAction as MessageNavAction,
        );
      },
      focusComposer: () => focusComposerRef.current?.(),
    };
    messageNavStore.registerContext(ctx);
    return () => messageNavStore.unregisterContext(ctx);
    // Register once per mount; callbacks read refs, and re-registering would
    // reset nav state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [!!focusComposer]);

  // A conversation switch resets navigation.
  useEffect(() => {
    if (messageNavStore.active) {
      messageNavStore.dispatch({ type: "cancel" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId]);

  // Project logical focus onto real DOM focus (native activation and screen
  // readers). Falls back to an awaited reveal when the row is off-window (#874).
  const navState = messageNavStore.state;
  useEffect(() => {
    if (navState.mode === "composer") {
      return;
    }
    const project = (row: HTMLElement) => {
      const target =
        navState.mode === "action"
          ? (row.querySelector<HTMLElement>(`[data-nav-action="${navState.action}"]`) ?? row)
          : row;
      target.focus({ preventScroll: true });
      row.scrollIntoView({ block: "nearest" });
    };
    const rendered = findRow(rowWindowRef.current, navState.messageId);
    if (rendered) {
      project(rendered);
      return;
    }
    let cancelled = false;
    void revealRow(
      rowWindowRef.current,
      navState.messageId,
      "auto",
      () => cancelled,
    ).then((row) => {
      if (row && !cancelled) {
        project(row);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [navState]);

  const handleNavKeyDown = (e: React.KeyboardEvent) => {
    if (!messageNavStore.active) {
      return;
    }
    // An open more-menu owns the keyboard.
    if (containerRef.current?.querySelector('[data-testid="message-actions-menu"]')) {
      return;
    }
    // stopPropagation on claimed keys: the window-level registry binds Escape to
    // nav.back.
    if (e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      messageNavStore.dispatch({ type: "up" });
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      e.stopPropagation();
      messageNavStore.dispatch({ type: "down" });
    } else if (isHorizontalArrow(e.key)) {
      e.preventDefault();
      e.stopPropagation();
      const step = arrowStep(e.key, isRtlElement(containerRef.current));
      messageNavStore.dispatch({ type: step > 0 ? "nextAction" : "prevAction" });
    } else if (e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      messageNavStore.dispatch({ type: "exit" });
    }
  };

  const handleNavBlur = (e: React.FocusEvent) => {
    if (!messageNavStore.active) {
      return;
    }
    const next = e.relatedTarget as Node | null;
    if (next && containerRef.current?.contains(next)) {
      return;
    }
    // Focus left the log; wherever it went owns it now.
    messageNavStore.dispatch({ type: "cancel" });
  };

  // Dividers and grouping come from the FULL timeline by index, never the slice.
  const renderTimelineItem = (idx: number) => {
    const item = timeline[idx];
    const prev = idx > 0 ? timeline[idx - 1] : null;
    const currentDay = startOfLocalDay(new Date(item.ts));
    const prevDay = prev ? startOfLocalDay(new Date(prev.ts)) : null;
    const showDivider =
      item.kind === "message" && (prevDay === null || prevDay !== currentDay);

    // Refined group start: first item, after a divider, a different author, or
    // a gap over GROUP_GAP_MS.
    const isGroupStart =
      item.kind === "message" &&
      (prev === null ||
        showDivider ||
        prev.message.sender_id !== item.message.sender_id ||
        item.ts - prev.ts > GROUP_GAP_MS);

    const { message } = item;
    const rendered = blockedIds.has(message.sender_id) ? (
      <div data-testid={`message-blocked-${message.id}`} className="px-4 py-1">
        <div className="flex items-start gap-2 min-w-0">
          <span
            className="flex-shrink-0 font-mono text-sm text-dim"
          >
            {t("list.blockedAuthor")}
          </span>
          <span
            className="font-mono text-sm text-muted"
          >
            {t("list.blockedBody")}
          </span>
        </div>
      </div>
    ) : (
      <MessageItem
        message={message}
        ctx={renderCtx}
        replyToMessage={
          message.reply_to_message_id
            ? (replyTargets.get(message.reply_to_message_id) ?? null)
            : undefined
        }
        authorUsername={
          getAuthorUsername
            ? getAuthorUsername(message.sender_id, message)
            : t("list.unknownAuthor")
        }
        isAuthorAdmin={adminUserIds?.has(message.sender_id) ?? false}
        isAuthorNew={isNewMemberMessage(
          newMemberJoinMs,
          message.sender_id,
          toMs(message.created_at),
        )}
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
        isPinned={pinnedMessageIds.has(message.id)}
        onTogglePin={pinsConversationId ? handleTogglePin : undefined}
        onCopyLink={handleCopyLink}
        onReport={handleReport}
        copyLinkState={
          copyLinkState?.messageId === message.id ? copyLinkState.state : "idle"
        }
        receipt={receipts?.get(message.id)}
        peerCount={peerCount}
        isDm={isDm}
      />
    );

    return (
      <>
        {showDivider && (
          <DayDivider
            label={formatDayDivider(toMs(message.created_at))}
            refined={skin === "refined"}
          />
        )}
        {rendered}
      </>
    );
  };

  if (timeline.length === 0) {
    return (
      <EmptyState testId="empty-messages">{t("list.empty")}</EmptyState>
    );
  }

  const virtualItems = virtualizer.getVirtualItems();

  return (
    <div
      data-testid="message-list"
      ref={containerRef}
      className="flex-1 overflow-y-auto min-h-0 bg-bg"
      onKeyDown={handleNavKeyDown}
      onBlur={handleNavBlur}
    >
      {isFetchingMore && (
        <p
          data-testid="message-loading-older"
          className="text-xs font-mono text-center py-2 text-muted"
        >
          {t("common:states.loading")}
        </p>
      )}
      <div
        data-testid="message-window"
        className="relative"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {virtualItems.map((virtualRow) => (
          // Absolute placement keeps the divider's margins inside the row's
          // measurement; the hover bar and menu still position against the row.
          <div
            key={virtualRow.key}
            data-index={virtualRow.index}
            ref={virtualizer.measureElement}
            style={{
              position: "absolute",
              insetInlineStart: 0,
              insetInlineEnd: 0,
              top: `${virtualRow.start}px`,
            }}
          >
            {renderTimelineItem(virtualRow.index)}
          </div>
        ))}
      </div>
      {isFetchingNewer && (
        <p
          data-testid="message-loading-newer"
          className="text-xs font-mono text-center py-2 text-muted"
        >
          {t("common:states.loading")}
        </p>
      )}
    </div>
  );
});
