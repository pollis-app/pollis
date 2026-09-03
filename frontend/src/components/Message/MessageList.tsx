import React, { useCallback, useEffect, useRef, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
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
import { useGroupMembers } from "../../hooks/queries/useGroups";
import { observer } from "mobx-react-lite";
import { rosterChangeStore, type RosterBanner } from "../../stores/rosterChangeStore";
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
import { EmptyState } from "../ui/EmptyState";

// How long the permalink jump highlight stays on the target row.
const HIGHLIGHT_MS = 1800;

const startOfLocalDay = (d: Date): number =>
  new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

// Time gap (ms) beyond which a same-author message still starts a new group.
const GROUP_GAP_MS = 5 * 60 * 1000;

// Memoised for the same reason the rows are: there is one of these per day in
// the log and their inputs are two primitives that almost never change.
const DayDivider: React.FC<{ label: string; refined: boolean }> = React.memo(({ label, refined }) => {
  if (refined) {
    // Centered mono label between two hairline rules, with density-driven
    // breathing room above and below.
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

const RosterChangeBanner: React.FC<{
  banner: RosterBanner;
  resolveName: (userId: string) => string;
}> = React.memo(({ banner, resolveName }) => {
  const { t } = useTranslation("chat");
  const name = resolveName(banner.payload.user_id);
  let label: string;
  switch (banner.payload.kind) {
    case "joined":
      label = t("roster.joined", { name });
      break;
    case "left":
      label = t("roster.left", { name });
      break;
    case "device_added":
      label = t("roster.deviceAdded", { name });
      break;
    case "device_removed":
      label = t("roster.deviceRemoved", { name });
      break;
  }
  return (
    <div
      data-testid={`roster-banner-${banner.id}`}
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
RosterChangeBanner.displayName = "RosterChangeBanner";

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
  onScrollToMessage?: (messageId: string) => void;
  getAuthorUsername?: (authorId: string, message?: Message) => string;
  hasMore?: boolean;
  isFetchingMore?: boolean;
  onLoadMore?: () => void;
  /** The log is an anchored window (#1039): there are messages newer than
   * its last row that are not the live tail. Scrolling near the bottom asks
   * for them via `onLoadNewer`, and arriving messages do not pull the
   * viewport down until the window has rejoined the tail. */
  hasNewer?: boolean;
  isFetchingNewer?: boolean;
  onLoadNewer?: () => void;
  /** Bumped by the parent whenever it replaces the log wholesale with the
   * live tail (leaving an anchored window), so the log re-opens at its newest
   * row exactly as it does on a conversation switch. */
  windowEpoch?: number;
  /** Opting into arrow-key log navigation: return focus to this surface's
   * composer. Only the surface that owns the app's main composer passes it —
   * exactly one mounted list may lend the nav store its context. */
  focusComposer?: () => void;
  /** The channel or DM id pins are recorded against (#99) — NOT the MLS
   * group id `conversationId` carries. Omit on surfaces without pinning
   * (search results, threads) and the affordance doesn't render. */
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
  const resolveName = useCallback(
    (userId: string) => usernameByUserId.get(userId) ?? userId,
    [usernameByUserId],
  );

  // ── List-wide render inputs, subscribed ONCE (#874) ─────────────────────
  // Every one of these used to be resolved per row: `useSkin()` and
  // `useMentionCandidates()` inside each `MessageItem`/`MessageBody` meant a
  // React Query observer and up to five MobX reactions PER MESSAGE, all
  // answering the same list-wide question. Subscribing here and passing one
  // memoised object down is not just cheaper — it is what makes the row memo
  // meaningful, because a fresh object per render would defeat it.
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

  // Reply targets resolved in ONE pass. Each row used to run
  // `allMessages.find()`, which is O(N^2) across the log (#874). Only messages
  // that are actually replied to are indexed, so this stays small.
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

  // ── Windowing (#874) ────────────────────────────────────────────────────
  // Only the visible slice of the timeline is in the DOM, so layout and paint
  // stop scaling with history length the way render cost already stopped.
  //
  // The list keeps its OWN scroll container and its own markup — the library
  // is headless, which is what lets day dividers, roster banners, sender
  // grouping and both skins' row shapes carry on unchanged inside the window.
  //
  // Timeline keys, not indexes, identify rows (`getItemKey`). That is what
  // makes prepending a page of older messages cheap: every row already
  // measured keeps its real height even though its index just shifted by 50.
  const timelineRef = useRef(timeline);
  timelineRef.current = timeline;
  const getItemKey = useCallback(
    (index: number) => timelineRef.current[index]?.key ?? index,
    [],
  );
  // Recomputed per render (one style read) rather than memoised, so it follows
  // a font-size change; the estimate only governs rows never yet on screen.
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
    // A chat log is anchored at its END, not its start. This is what replaces
    // the hand-rolled scroll-restore that used to bracket a load-more: the
    // library re-pins the row the user is looking at across any count change,
    // whether 50 older messages were prepended or a row above the fold turned
    // out taller than its estimate.
    anchorTo: "end",
    // Follow new messages down — but only from the bottom. Scrolled back
    // through history, an arriving message no longer yanks the viewport away,
    // which is how Slack and Discord both behave.
    //
    // Off entirely while the log is an anchored window (#1039): the "bottom"
    // of such a window is not the present, and a forward page appending
    // below it is history the reader has not seen yet, not a new arrival to
    // follow. Held off one commit past the last append too (`isFetchingNewer`
    // clears then), so the rejoining page is not followed either.
    followOnAppend: hasNewer || isFetchingNewer ? false : "smooth",
    // "At the bottom" with one row of slack, rather than the 1px default.
    scrollEndThreshold: rowEstimatePx,
  });

  // Message id → timeline index, for the three subsystems that have to ask for
  // a row before they can look at it (see `messageWindow.ts`).
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

  // Stable across renders and ref-backed, so a reveal that is still in flight
  // reads the live timeline rather than the one that existed when it started.
  const rowWindow = useMemo<MessageRowWindow>(
    () => ({
      container: () => containerRef.current,
      indexOf: (messageId: string) => indexByMessageIdRef.current.get(messageId),
      virtualizer,
    }),
    [virtualizer],
  );
  // Held in a ref as well so the row callbacks below keep the empty dependency
  // array that #874 made load-bearing — a fresh `onScrollToReply` identity
  // re-renders every row in the log.
  const rowWindowRef = useRef(rowWindow);
  rowWindowRef.current = rowWindow;

  // Open at the newest message.
  //
  // `followOnAppend` keeps the log pinned to the bottom from then on, but it
  // only reacts to the message COUNT changing while a scroll container exists
  // — and a conversation whose first page is already in hand renders its whole
  // log in one go, with nothing for it to react to. Without this, opening a
  // channel with any real history lands the reader at its very first message.
  //
  // Deliberately re-runs rather than latching on a ref: React re-attaches the
  // scroll element on its dev-mode double mount, and the virtualizer rewinds
  // to the offset it still believes in when that happens. An anchor that fires
  // once, first, loses that race — and losing it silently is exactly the shape
  // of bug that ships.
  const logKey = `${selectedChannelId ?? ""}|${selectedConversationId ?? ""}`;
  const hasRows = timeline.length > 0;
  useEffect(() => {
    if (!hasRows) {
      return;
    }
    virtualizer.scrollToIndex(timelineRef.current.length - 1, { align: "end" });
  }, [logKey, windowEpoch, hasRows, virtualizer]);

  // Ask for another page while the reader is at the top of the log — or while
  // there is no top to reach, because the log does not fill its own viewport.
  const maybeLoadMore = useCallback(() => {
    const container = containerRef.current;
    if (!container || !hasMore || isFetchingMore) {
      return;
    }
    // Recomputed here, not captured: the threshold is in rem, so it follows a
    // font-size change under a mounted log (#934).
    const threshold = loadMoreThresholdPx();
    const scrollable = container.scrollHeight - container.clientHeight > threshold;
    if (!scrollable || container.scrollTop < threshold) {
      onLoadMore?.();
    }
  }, [hasMore, isFetchingMore, onLoadMore]);

  // Trigger load-more when the user scrolls near the top.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    container.addEventListener("scroll", maybeLoadMore, { passive: true });
    return () => container.removeEventListener("scroll", maybeLoadMore);
  }, [maybeLoadMore]);

  // …and again whenever the log's contents change.
  //
  // Scrolling is the only thing that ever asks for history, and a viewport
  // already at offset 0 — or one the whole first page fits inside — has no
  // scrolling left to give: the browser emits no event for a wheel that
  // cannot move anything. A prepend that lands the reader back at the top
  // therefore used to dead-end the log on the page it had, with the rest of
  // the conversation sitting decryptable on disk and unreachable (#927).
  //
  // The conditions inside `maybeLoadMore` are what keep this from running
  // away: the reader has to still be at the top, so the ordinary prepend —
  // which re-anchors them onto the row they were reading, well below it —
  // stops after one page. The anchoring runs in a layout effect, i.e. before
  // this one, so `scrollTop` here is already the post-prepend truth. Opening
  // a conversation is safe for the same reason: the log has scrolled itself
  // to its newest message by the time this runs.
  useEffect(() => {
    maybeLoadMore();
  }, [maybeLoadMore, timeline.length]);

  // The mirror image, for an anchored window (#1039): ask for the next page
  // toward the present while the reader is at the BOTTOM of the window — or
  // while the window does not fill its viewport, for the same reason as above.
  // `hasNewer` is false in live mode, so this is inert on the ordinary log.
  //
  // Runaway is bounded the same way: the append re-pins the reader onto the
  // row they were reading (`followOnAppend` is off while `hasNewer`), which
  // is no longer near the bottom once a page sits below it.
  //
  // Not while a jump is still pending: the window has just been re-seated
  // and the viewport is wherever the previous log left it (typically its
  // bottom, i.e. this window's bottom) until the jump effect below centres
  // the target. Asking then would fetch a page nobody has scrolled toward.
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

  // `useCallback` is load-bearing, not tidiness: this is handed to every row as
  // `onScrollToReply`, so a fresh identity per render re-rendered the entire log
  // on any parent state change — one keystroke in the edit bar was N row
  // renders (#874). Reads the DOM and a ref only, so it has no dependencies.
  //
  // #874: the quoted parent is very often OFF the window — that is the whole
  // point of quoting it — so this can no longer be a bare `querySelector`. It
  // asks the window for the row first and only then scrolls.
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

  // ── Saved messages + permalinks (#854) ──────────────────────────────────
  const savedMessageIds = useSavedMessageIds();
  const toggleSavedMutation = useToggleSavedMessage();
  // ── Pinned messages (#99): one query + one id-set for the whole list. ────
  const pinnedMessageIds = usePinnedMessageIds(pinsConversationId);
  const togglePinMutation = useTogglePinnedMessage();
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

  // Both row callbacks below are pinned to a stable identity via refs rather
  // than dependency arrays (#874). `useMutation` returns a NEW result object
  // every render, and `sortedMessages` changes whenever any message arrives —
  // either as a dependency would hand all N rows a new prop and re-render the
  // whole log. Refs are read at call time, so the behaviour is identical while
  // the identity never changes.
  const toggleSavedRef = useRef(toggleSavedMutation);
  toggleSavedRef.current = toggleSavedMutation;
  const handleToggleSave = useCallback((messageId: string) => {
    toggleSavedRef.current.mutate(messageId);
  }, []);

  // Same ref-pinning as the bookmark toggle, same reason (#874).
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
      const ok = await copyPermalinkRef.current(targetConversationId, messageId);
      setCopyLinkState({ messageId, state: ok ? "copied" : "failed" });
    },
    [],
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
  //
  // #874: a permalink almost by definition points at something old, so the
  // target is usually outside the window. `revealRow` moves the window there
  // and waits for the commit; the jump is only claimed once a real row exists,
  // which keeps the "not loaded yet, stay pending" behaviour intact.
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

  // ── Arrow-key log navigation (bash-history style) ───────────────────────
  // The machine lives in `utils/messageNav.ts`, the live state in
  // `stores/messageNavStore.ts`; this component lends the store its context
  // (rendered order + per-row action bars), projects the logical focus onto
  // real DOM focus, and translates key events. Rows style themselves purely
  // via CSS `focus-within`, so keystrokes re-render nothing.

  // Blocked-author rows render as inert stubs with no hover actions — skip
  // them when walking the log, like tombstones they are not actionable, but
  // unlike tombstones there is nothing to read either.
  const navigableIds = useMemo(
    () => sortedMessages.filter((m) => !blockedIds.has(m.sender_id)).map((m) => m.id),
    [sortedMessages, blockedIds],
  );
  const navigableIdsRef = useRef(navigableIds);
  navigableIdsRef.current = navigableIds;
  const focusComposerRef = useRef(focusComposer);
  focusComposerRef.current = focusComposer;

  useEffect(() => {
    // Presence of `focusComposer` is the opt-in — surfaces without it (search
    // results, future thread lists) never touch the nav store.
    if (!focusComposer) {
      return;
    }
    const ctx = {
      orderedIds: () => navigableIdsRef.current,
      // The RENDERED action bar is the source of truth for what this row
      // offers — reading the DOM here means the nav machine can never select
      // a button that isn't on screen (deleted rows have no bar at all).
      // Only ever asked about the row the projection above just focused, which
      // is on screen by construction — so this stays a plain synchronous read
      // even though the log is windowed.
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
    // Register once per mount; the callbacks read refs, so they always see
    // the latest list without re-registering (which would reset nav state).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [!!focusComposer]);

  // Switching conversations swaps the entire log out from under an in-flight
  // navigation — reset rather than leaving state pointing into the old list.
  useEffect(() => {
    if (messageNavStore.active) {
      messageNavStore.dispatch({ type: "cancel" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId]);

  // Project the logical focus onto real DOM focus: the row in `message` mode,
  // the specific bar button in `action` mode. Real focus keeps Enter/Space
  // activation and screen-reader tracking native.
  //
  // #874: walking off the top edge of the window lands on a row that is not in
  // the document. The synchronous path is kept for the overwhelmingly common
  // case (the next row is already rendered, thanks to the overscan) and only
  // falls back to an awaited reveal when it genuinely is not.
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
    // While a row's more-menu is open it owns the keyboard (its own Escape
    // closes it); swallowing nothing here keeps that self-contained.
    if (containerRef.current?.querySelector('[data-testid="message-actions-menu"]')) {
      return;
    }
    // stopPropagation on every claimed key: the global shortcut registry
    // listens at window bubble (Escape = nav.back, which would navigate the
    // whole channel away), and a claimed key must never double-fire there.
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
    // Focus left the log for somewhere else (a click, an activated action
    // that opened the reply/edit bar) — the session is over, and wherever
    // focus went now legitimately owns it.
    messageNavStore.dispatch({ type: "cancel" });
  };

  // One timeline entry, dividers and grouping included. Both are computed from
  // the FULL timeline by index rather than from the rendered slice, so a day
  // divider or a grouped follow-up looks the same whether or not its neighbour
  // happens to be inside the window.
  const renderTimelineItem = (idx: number) => {
    const item = timeline[idx];
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
        <RosterChangeBanner banner={item.banner} resolveName={resolveName} />
      );
    }

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
      {/* The full height of the whole history, holding the scrollbar steady,
          with only the visible slice of rows actually inside it. */}
      <div
        data-testid="message-window"
        className="relative"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {virtualItems.map((virtualRow) => (
          // Absolute placement is what makes the row's position independent of
          // its neighbours, and it establishes a block formatting context, so
          // the refined day divider's margins are measured INSIDE the row
          // rather than escaping it and leaving every measurement short.
          //
          // It does NOT change how the row's own overflowing chrome paints:
          // the hover action bar and its `z-40` menu are positioned against
          // the row's existing `relative` box, exactly as before, and nothing
          // here clips (which is what ruled out `content-visibility` in
          // phase 2 — see `.codesight/wiki/ui.md`).
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
