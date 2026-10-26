import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Text, FlatList } from "react-native";
import { useFocusEffect, useLocalSearchParams, useRouter, type Href } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, IconButton } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  GROUP_WINDOW_MS,
  dayKey,
  dayLabel,
  timeLabel,
} from "../../components/chat/dates";
import { DaySeparator } from "../../components/chat/DaySeparator";
import { UndecryptableRow } from "../../components/chat/UndecryptableRow";
import { collapseUndecryptable } from "../../lib/collapseUndecryptable";
import { MessageRow } from "../../components/chat/MessageRow";
import { Composer } from "../../components/chat/Composer";
import { authorName } from "../../lib/authorName";
import { outgoingText } from "../../lib/messageText";
import { EditBar } from "../../components/chat/EditBar";
import { ReplyBar } from "../../components/chat/ReplyBar";
import { replySnippet, replyTargets } from "../../lib/replyPreview";
import { MessageActionsSheet } from "../../components/chat/MessageActionsSheet";
import { ChannelMenuSheet } from "../../components/chat/ChannelMenuSheet";
import { EmojiPickerSheet } from "../../components/emoji/EmojiPickerSheet";
import { afterSheetClose } from "../../components/chat/SheetOverlay";
import {
  useMessages,
  useSendMessage,
  useRetryMessage,
  useDiscardUnsent,
  useIngestConversation,
  useToggleReaction,
  useConversationReactions,
  useEditMessage,
  useDeleteMessage,
  useConversationReceipts,
  useSendReadReceipts,
  useThreadSummaries,
  useSavedMessageIds,
  useToggleSavedMessage,
  usePinnedMessageIds,
  useTogglePinnedMessage,
  useUserGroupsWithChannels,
  useGroupMembers,
  flattenPages,
  markConversationRead,
  type ConversationKind,
  type Message,
} from "../../hooks/queries";
import { retryTarget } from "../../hooks/queries/pendingMessages";
import { useConversationRealtime } from "../../hooks/useConversationRealtime";
import { useReadReceipts } from "../../hooks/useReadReceipts";
import { useMentionCandidates } from "../../hooks/useMentionCandidates";
import { useUsableEmoji } from "../../hooks/queries/useEmoji";
import * as Clipboard from "expo-clipboard";
import * as ImagePicker from "expo-image-picker";
import { formatMessagePermalink } from "../../lib/permalinks";
import type { PickedAttachment } from "../../lib/attachments";
import { ensurePushRegistration } from "../../lib/push";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useNav } from "../../components/pane/paneContext";
import { useOpenMediaViewer } from "../../hooks/useOpenMediaViewer";
import { useIsRegular } from "../../hooks/useLayoutClass";
import { selectConversation } from "../../hooks/useOpenConversation";
import { haptic } from "../../lib/haptics";
import { messageJumpStore } from "../../stores/messageJumpStore";
import { nextJumpStep } from "../../lib/messageJump";

// Props let this screen double as an embedded right-pane conversation on the
// two-pane (regular/iPad) layout. Route usage passes NO props, so every value
// falls back to the route params exactly as before.
type ChatViewProps = {
  conversationId?: string | null;
  kind?: ConversationKind | null;
  groupId?: string;
  name?: string;
  embedded?: boolean;
};

// Rows for the inverted timeline list — messages interleaved with day
// separators, newest first.
type ChatListItem =
  | { type: "sep"; key: string; label: string }
  | { type: "msg"; key: string; message: Message; continued: boolean };

function TextChat(props: ChatViewProps = {}) {
  // Embedded in the iPad two-pane, pushes of info / thread / profile pages
  // land in the detail pane (useNav); on phones it is the router.
  const router = useNav();
  const { t } = useTranslation("mobile");
  const params = useLocalSearchParams<{
    id?: string;
    kind?: string;
    name?: string;
  }>();
  const embedded = props.embedded ?? false;
  const conversationId = props.conversationId ?? params.id ?? null;
  const kind: ConversationKind | null =
    props.kind ??
    (params.kind === "channel" || params.kind === "dm" ? params.kind : null);
  const displayName =
    props.name ?? (typeof params.name === "string" ? params.name : undefined);
  const openMedia = useOpenMediaViewer(conversationId, kind);

  const [draft, setDraft] = useState("");
  const [pendingAttachments, setPendingAttachments] = useState<
    PickedAttachment[]
  >([]);
  const [actionTarget, setActionTarget] = useState<Message | null>(null);
  const [pickerTarget, setPickerTarget] = useState<Message | null>(null);
  const [editTarget, setEditTarget] = useState<Message | null>(null);
  const [editDraft, setEditDraft] = useState("");
  // The message the composer is replying to (#1270). Swipe, double tap, or
  // the message sheet sets it; sending, dismissing or starting an edit
  // clears it.
  const [replyTarget, setReplyTarget] = useState<Message | null>(null);
  // Bumped to focus the composer when a reply starts.
  const [composerFocus, setComposerFocus] = useState(0);
  // The row a tapped reply quote jumped to, tinted for a moment.
  const [highlightId, setHighlightId] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const currentUser = appStore.currentUser;

  const {
    data,
    isLoading,
    isError,
    fetchNextPage,
    hasNextPage,
    isFetchingNextPage,
  } = useMessages(conversationId, kind);
  // Newest-first (matches the inverted list's render order).
  const messages = useMemo(() => flattenPages(data), [data]);
  const messageIds = useMemo(
    () => messages.filter((m) => !m.pending && !m.failed).map((m) => m.id),
    [messages],
  );
  const { data: reactionsByMessage } = useConversationReactions(
    conversationId,
    kind,
    messageIds,
  );

  // DM receipts (#892): one fetch per open conversation; mark-read reporting
  // via the list's viewability config, gated on the reciprocal synced
  // preference. Channels render nothing and report nothing.
  const isDm = kind === "dm";
  const sendReadReceipts = useSendReadReceipts();
  const { data: receiptsByMessage } = useConversationReceipts(
    isDm ? conversationId : null,
  );
  const { viewabilityConfigCallbackPairs } = useReadReceipts(
    conversationId,
    currentUser?.id ?? null,
    isDm && sendReadReceipts,
  );
  // Mobile DMs are strictly 1:1 (desktop derives from member_count with the
  // same 2-member fallback).
  const peerCount = 1;
  const sendMessage = useSendMessage(conversationId, kind);
  const retryMessage = useRetryMessage(conversationId, kind);
  const discardUnsent = useDiscardUnsent(conversationId, kind);
  const ingest = useIngestConversation();
  const toggleReaction = useToggleReaction(conversationId, kind);
  const editMessage = useEditMessage(conversationId, kind);
  const deleteMessage = useDeleteMessage(conversationId, kind);

  // Foreground realtime — supplements the focus ingest below with a live
  // data-channel subscription so peer messages land without a refocus. The
  // group room is named by group_id (set on the store when a channel was
  // opened); DMs use the conversation_id directly. No-op when realtime is
  // unavailable.
  const groupId =
    props.groupId ??
    (kind === "channel" ? appStore.selectedGroupId ?? undefined : undefined);
  useConversationRealtime(conversationId, kind, groupId);

  // Contextual notification permission: opening a conversation is the first
  // moment notifications are obviously useful, so ask here rather than at
  // login. Best-effort and one-shot per session — `ensurePushRegistration`
  // pre-checks status and won't re-prompt once answered.
  useEffect(() => {
    const uid = currentUser?.id;
    if (!uid) {
      return;
    }
    void ensurePushRegistration(uid);
  }, [currentUser?.id]);

  // Trigger ingest on screen focus — covers the "returning to a chat after
  // the app was backgrounded" case where the periodic refetch hasn't fired
  // yet. The query invalidation inside `useIngestConversation` refreshes
  // the visible list once new envelopes have been decrypted.
  useFocusEffect(
    useCallback(() => {
      if (conversationId && kind) {
        void ingest(conversationId, kind);
      }
    }, [conversationId, kind, ingest]),
  );

  // Unread (#1422): this conversation is on screen, so it never badges and
  // its persisted read cursor advances — on arrival, however it was reached
  // (a row, Search, a notification tap), and again on leaving, for whatever
  // landed while it was open. The newest-message effect below covers arrivals
  // while it stays open.
  useFocusEffect(
    useCallback(() => {
      if (!conversationId) {
        return;
      }
      appStore.setOpenConversationId(conversationId);
      markConversationRead(conversationId);
      return () => {
        appStore.clearOpenConversationId(conversationId);
        markConversationRead(conversationId);
      };
    }, [conversationId]),
  );

  // Auto-scroll to bottom when a NEW newest message lands (arrival or
  // optimistic send). Keyed on the newest id rather than the array length so
  // loading an older page never yanks the reader away from the history they
  // are reading.
  const listRef = useRef<FlatList<ChatListItem>>(null);
  const newestId = messages[0]?.id;
  useEffect(() => {
    if (!newestId) {
      return;
    }
    requestAnimationFrame(() => {
      listRef.current?.scrollToOffset({ offset: 0, animated: true });
    });
  }, [newestId]);

  // A message that arrives while the conversation is on screen is read: move
  // the cursor so the persisted count agrees once this screen is left.
  useEffect(() => {
    if (!conversationId || !newestId) {
      return;
    }
    if (appStore.openConversationId !== conversationId) {
      return;
    }
    markConversationRead(conversationId);
  }, [conversationId, newestId]);

  // The embedded (iPad) pane keeps this component mounted across
  // conversations, so a reply must never outlive the one it was started in.
  useEffect(() => {
    setReplyTarget(null);
    setHighlightId(null);
  }, [conversationId]);

  const startReply = useCallback((m: Message) => {
    setEditTarget(null);
    setEditDraft("");
    setReplyTarget(m);
    setComposerFocus((n) => n + 1);
  }, []);

  const onSend = () => {
    const text = outgoingText(draft);
    if ((!text && pendingAttachments.length === 0) || sendMessage.isPending) {
      return;
    }
    setDraft("");
    setPendingAttachments([]);
    setReplyTarget(null);
    sendMessage.mutate({
      content: text,
      attachments: pendingAttachments,
      replyToId: replyTarget?.id,
    });
  };

  // Attach images from the library (#894-adjacent; same upload flow as
  // desktop — Rust encrypts + uploads on send).
  const onAttach = useCallback(async () => {
    const result = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: "images",
      allowsMultipleSelection: true,
      quality: 1,
    });
    if (result.canceled) {
      return;
    }
    const picked: PickedAttachment[] = result.assets.map((asset, i) => ({
      id: asset.assetId ?? `${Date.now()}-${i}`,
      uri: asset.uri,
      name: asset.fileName ?? `image-${Date.now()}-${i}.jpg`,
      mimeType: asset.mimeType ?? "image/jpeg",
      width: asset.width,
      height: asset.height,
    }));
    setPendingAttachments((prev) => [...prev, ...picked]);
  }, []);

  const onSaveEdit = () => {
    const text = outgoingText(editDraft);
    if (!text || !editTarget) {
      return;
    }
    editMessage.mutate(
      { messageId: editTarget.id, newContent: text },
      {
        onSuccess: () => {
          setEditTarget(null);
          setEditDraft("");
        },
      },
    );
  };

  // Reply-count chips for thread roots (#831).
  const { data: threadSummaries } = useThreadSummaries(conversationId);

  // Mention candidates come from the visible roster only (#886); the
  // resolution set for rendering adds `all` and the reader's own name so
  // self-mentions highlight.
  // The composer resolves `:shortcode:` against these before send, which is
  // also the set the picker offers — one source, one permission rule.
  const { data: usableEmoji } = useUsableEmoji();
  const mentionCandidates = useMentionCandidates(
    kind,
    conversationId,
    groupId ?? null,
  );
  // Saved messages (#887): one query feeds every row's saved state.
  const savedIds = useSavedMessageIds();
  const toggleSaved = useToggleSavedMessage();
  // Pinned messages (#99, #1315): one query feeds the sheet's Pin / Unpin.
  // Keyed on the channel or DM id, which is this screen's `conversationId`.
  const pinnedIds = usePinnedMessageIds(conversationId);
  const togglePin = useTogglePinnedMessage();

  // #897: copy is VERIFIED — the boolean comes from the clipboard call, and
  // a thrown write reads as failure, never assumed success.
  const copyToClipboard = useCallback(async (text: string) => {
    let copied = false;
    try {
      copied = await Clipboard.setStringAsync(text);
    } catch {
      copied = false;
    }
    // A copy confirms by touch, and a failed one says so (#1286).
    haptic(copied ? "success" : "error");
    return copied;
  }, []);

  const selfName = currentUser?.username?.toLowerCase() ?? null;
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

  // Inverted-list items: build chronologically (day separator before the
  // first message of each day), then reverse so index 0 is the newest row.
  // Thread replies stay out of the main timeline — they render only in the
  // thread screen (#837's isolation fix; same filter as desktop's
  // `!m.thread_id || m.thread_id === m.id` render boundary).
  const items = useMemo(() => {
    // A run of lost messages (#1312) is one row, as on desktop.
    const chrono = collapseUndecryptable(
      [...messages].reverse().filter((m) => !m.thread_id || m.thread_id === m.id),
    );
    const out: ChatListItem[] = [];
    let lastKey = "";
    let prev: Message | null = null;
    for (const m of chrono) {
      const k = dayKey(m.created_at);
      if (k !== lastKey) {
        out.push({ type: "sep", key: `sep-${k}`, label: dayLabel(m.created_at) });
        lastKey = k;
        prev = null;
      }
      // Discord-style grouping: the same sender again within the window
      // (and on the same day) hangs under the previous header. A reply
      // always starts its own group, so its quote sits over a header.
      const continued =
        prev !== null &&
        !m.reply_to_id &&
        prev.sender_id === m.sender_id &&
        m.created_at - prev.created_at < GROUP_WINDOW_MS;
      out.push({ type: "msg", key: m.id, message: m, continued });
      prev = m;
    }
    out.reverse();
    return out;
  }, [messages]);

  // The loaded originals of every reply, in one pass (desktop's #874 shape).
  const repliedTo = useMemo(() => replyTargets(messages), [messages]);

  // Jump to a replied-to message and tint it briefly. Only loaded messages
  // are reachable; the quote of an unloaded one is not pressable.
  const highlightTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (highlightTimer.current) {
        clearTimeout(highlightTimer.current);
      }
    },
    [],
  );
  const scrollToMessage = useCallback(
    (messageId: string) => {
      const index = items.findIndex((i) => i.type === "msg" && i.key === messageId);
      if (index < 0) {
        return;
      }
      listRef.current?.scrollToIndex({ index, animated: true, viewPosition: 0.5 });
      setHighlightId(messageId);
      if (highlightTimer.current) {
        clearTimeout(highlightTimer.current);
      }
      highlightTimer.current = setTimeout(() => setHighlightId(null), 1400);
    },
    [items],
  );
  // Rows far off screen have no measured position yet: get near by the
  // average row height, then aim again once those rows have laid out.
  const onScrollToIndexFailed = useCallback(
    (info: { index: number; averageItemLength: number }) => {
      listRef.current?.scrollToOffset({
        offset: info.averageItemLength * info.index,
        animated: false,
      });
      setTimeout(() => {
        listRef.current?.scrollToIndex({
          index: info.index,
          animated: true,
          viewPosition: 0.5,
        });
      }, 120);
    },
    [],
  );

  // A jump queued by the pinned list, Saved or a permalink
  // (stores/messageJumpStore.ts): page back until the row is rendered, then
  // scroll to it, tint it, and claim the jump so it fires once.
  const jumpConversationId = messageJumpStore.conversationId;
  const jumpMessageId = messageJumpStore.messageId;
  const pageCount = data?.pages.length ?? 0;
  useEffect(() => {
    if (!jumpMessageId || jumpConversationId !== conversationId) {
      return;
    }
    const step = nextJumpStep({
      index: items.findIndex((i) => i.type === "msg" && i.key === jumpMessageId),
      loaded: messages.some((m) => m.id === jumpMessageId),
      isLoading,
      isFetchingNextPage,
      hasNextPage: !!hasNextPage,
      pageCount,
    });
    if (step.kind === "scroll") {
      messageJumpStore.claim(jumpMessageId);
      // On the next frame: the list has no layout for a just-rendered row.
      requestAnimationFrame(() => scrollToMessage(jumpMessageId));
    } else if (step.kind === "loadOlder") {
      void fetchNextPage();
    } else if (step.kind === "giveUp") {
      messageJumpStore.reset();
    }
  }, [
    jumpConversationId,
    jumpMessageId,
    conversationId,
    items,
    messages,
    isLoading,
    isFetchingNextPage,
    hasNextPage,
    pageCount,
    fetchNextPage,
    scrollToMessage,
  ]);

  const onEndReached = useCallback(() => {
    if (hasNextPage && !isFetchingNextPage) {
      void fetchNextPage();
    }
  }, [hasNextPage, isFetchingNextPage, fetchNextPage]);

  // Toggle a reaction by its CURRENT state — tapping a pill you're in
  // removes, anything else adds (desktop's toggle semantics).
  const reactWithEmoji = useCallback(
    (messageId: string, emoji: string) => {
      const existing = reactionsByMessage
        ?.get(messageId)
        ?.find((reaction) => reaction.emoji === emoji);
      const reacted = currentUser
        ? existing?.user_ids.includes(currentUser.id) ?? false
        : false;
      toggleReaction.mutate({
        messageId,
        emoji,
        mode: reacted ? "remove" : "add",
      });
    },
    [reactionsByMessage, currentUser, toggleReaction],
  );

  // Header context for a channel: its group's name (subtitle + "Back to"),
  // the member count, and the channel's own name when the opener passed none
  // (the two-pane groups layout embeds this view without a name).
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = useMemo(
    () =>
      kind === "channel" && groupId
        ? groupsWithChannels?.find((g) => g.id === groupId) ?? null
        : null,
    [kind, groupId, groupsWithChannels],
  );
  const channelName = useMemo(
    () => group?.channels.find((c) => c.id === conversationId)?.name ?? null,
    [group, conversationId],
  );
  const { data: groupMembers } = useGroupMembers(
    kind === "channel" ? groupId ?? null : null,
  );

  // Header title: prefer the human name passed in by the opener (channel name
  // or DM peer handle). For DMs opened without one, fall back to the other
  // participant's username derived from the messages. Never show the raw
  // conversation ULID — that's what was overflowing the context bar.
  const peerName = useMemo(() => {
    if (kind !== "dm") {
      return null;
    }
    const peerMsg = messages.find((m) => m.sender_id !== currentUser?.id);
    return peerMsg?.sender_username ?? null;
  }, [kind, messages, currentUser?.id]);

  const title =
    (displayName && displayName.trim()) ||
    channelName ||
    peerName ||
    (kind === "dm"
      ? t("dms:conversation.fallbackTitle")
      : t("channels:channel.fallbackTitle"));

  const subtitle =
    kind === "channel" && group
      ? groupMembers && groupMembers.length > 0
        ? `${group.name} · ${t("group.detail.memberCount", {
            count: groupMembers.length,
          })}`
        : group.name
      : undefined;
  const backTo =
    kind === "dm" ? t("tabs.direct") : group?.name ?? undefined;

  const openThread = useCallback(
    (rootId: string) => {
      if (!conversationId || !kind) {
        return;
      }
      router.push({
        pathname: "/chat/thread",
        params: {
          threadId: rootId,
          id: conversationId,
          kind,
          name: title,
        },
      });
    },
    [conversationId, kind, router, title],
  );

  const renderItem = useCallback(
    ({ item }: { item: ChatListItem }) => {
      if (item.type === "sep") {
        return <DaySeparator label={item.label} />;
      }
      const m = item.message;
      if (m.undecryptable) {
        return (
          <UndecryptableRow
            messageId={m.id}
            time={timeLabel(m.created_at)}
            count={m.undecryptableCount ?? 1}
            onOpenDiagnostics={() => router.push("/self/diagnostics")}
          />
        );
      }
      const mine = currentUser?.id === m.sender_id;
      const summary = threadSummaries?.get(m.id);
      const name =
        authorName(m.sender_id, m.sender_username, currentUser) ??
        t("chat:list.unknownAuthor");
      const original = m.reply_to_id ? repliedTo.get(m.reply_to_id) : undefined;
      const replyTo = m.reply_to_id
        ? original
          ? {
              name:
                authorName(original.sender_id, original.sender_username, currentUser) ??
                t("chat:list.unknownAuthor"),
              snippet: replySnippet(original),
            }
          : null
        : undefined;
      const settled = !m.pending && !m.failed;
      // A failed send (#1335) offers Retry when the core stored it.
      const retryId = retryTarget(m);
      return (
        <MessageRow
          testID={`row-message-${m.id}`}
          messageId={m.id}
          av={name.slice(0, 2)}
          amber={mine}
          name={name}
          time={timeLabel(m.created_at)}
          text={m.content}
          attachments={m.attachments}
          onOpenAttachment={openMedia}
          pending={m.pending}
          failed={m.failed}
          onRetry={retryId ? () => retryMessage.mutate(retryId) : undefined}
          onDiscard={m.failed ? () => discardUnsent.mutate(m.outboxId ?? m.id) : undefined}
          edited={!!m.edited_at}
          reactions={reactionsByMessage?.get(m.id)}
          currentUserId={currentUser?.id}
          receipt={receiptsByMessage?.get(m.id)}
          peerCount={peerCount}
          showReceipt={mine && isDm}
          continued={item.continued}
          threadCount={summary?.reply_count ?? 0}
          threadLastReply={
            summary?.last_reply_at
              ? timeLabel(new Date(summary.last_reply_at).getTime())
              : undefined
          }
          onOpenThread={() => openThread(m.id)}
          mentionNames={mentionNames}
          selfName={selfName}
          onToggleReaction={(emoji, reacted) =>
            toggleReaction.mutate({
              messageId: m.id,
              emoji,
              mode: reacted ? "remove" : "add",
            })
          }
          onPressAvatar={
            mine
              ? undefined
              : () =>
                  router.push({
                    pathname: "/user/[id]",
                    params: { id: m.sender_id },
                  })
          }
          onLongPress={settled ? () => setActionTarget(m) : undefined}
          onReply={settled ? () => startReply(m) : undefined}
          replyTo={replyTo}
          onPressReplyQuote={original ? () => scrollToMessage(original.id) : undefined}
          highlighted={highlightId === m.id}
        />
      );
    },
    [
      currentUser,
      router,
      reactionsByMessage,
      toggleReaction,
      receiptsByMessage,
      peerCount,
      isDm,
      threadSummaries,
      openThread,
      openMedia,
      mentionNames,
      selfName,
      repliedTo,
      startReply,
      scrollToMessage,
      highlightId,
      retryMessage,
      discardUnsent,
      t,
    ],
  );

  const content = (
    <>
      <Header
        hideBack={embedded}
        backTo={backTo}
        title={title}
        subtitle={subtitle}
        titleIcon={
          kind === "channel" ? (
            <Icon.hash size={15} color={semantic.dim} />
          ) : undefined
        }
        actions={
          <>
            <IconButton
              testID="btn-pins"
              accessibilityLabel={
                kind === "dm"
                  ? t("dms:conversation.pinsLabel")
                  : t("channels:channel.pinsLabel")
              }
              icon={<Icon.pin size={22} color={semantic.text} />}
              onPress={
                conversationId && kind
                  ? () =>
                      router.push({
                        pathname: "/conversation/pins",
                        params: { id: conversationId, kind, name: title },
                      })
                  : undefined
              }
            />
            <IconButton
              testID="btn-members"
              accessibilityLabel={t("nav:panel.ariaLabel")}
              icon={<Icon.users size={22} color={semantic.text} />}
              onPress={
                conversationId && kind
                  ? () =>
                      router.push({
                        pathname: "/conversation/info",
                        params: {
                          id: conversationId,
                          kind,
                          name: title,
                          ...(groupId ? { groupId } : {}),
                        },
                      })
                  : undefined
              }
            />
            <IconButton
              testID="btn-chat-menu"
              accessibilityLabel={t("common:actions.moreOptions")}
              icon={<Icon.more size={22} color={semantic.text} />}
              onPress={() => {
                if (!conversationId) {
                  return;
                }
                if (kind === "dm") {
                  router.push({
                    pathname: "/dm/info",
                    params: { id: conversationId },
                  });
                  return;
                }
                if (kind === "channel") {
                  setMenuOpen(true);
                }
              }}
            />
          </>
        }
      />
      {isLoading && messages.length === 0 ? (
        <Text
          style={[
            ty.secondary,
            { color: semantic.muted, paddingHorizontal: 16, paddingTop: 12 },
          ]}
        >
          {t("chat.loading")}
        </Text>
      ) : null}
      {isError ? (
        <Text
          accessibilityRole="alert"
          style={[
            ty.secondary,
            { color: semantic.danger, paddingHorizontal: 16, paddingTop: 12 },
          ]}
        >
          {t("chat.loadFailed")}
        </Text>
      ) : null}
      {!isLoading && !isError && items.length === 0 ? (
        <Text
          style={[
            ty.secondary,
            { color: semantic.muted, paddingHorizontal: 16, paddingTop: 12 },
          ]}
        >
          {t("chat:list.empty")}
        </Text>
      ) : null}
      <FlatList
        ref={listRef}
        testID="list-messages"
        inverted
        data={items}
        keyExtractor={(item) => item.key}
        renderItem={renderItem}
        style={{ flex: 1 }}
        // `flexGrow` + `justifyContent` anchor a SHORT thread to the visual
        // top. Without them an inverted list pins its content to the visual
        // bottom, so a new or nearly-empty conversation renders its handful of
        // messages jammed against the composer with a large void under the
        // header. Note the value looks inverted because it is: `inverted`
        // flips the list, so `flex-end` in the flipped axis is the visual TOP.
        // A thread taller than the viewport is unaffected — `flexGrow` only
        // does anything while the content is shorter than the list.
        contentContainerStyle={{
          paddingTop: 4,
          paddingBottom: 12,
          flexGrow: 1,
          justifyContent: "flex-end",
        }}
        // With `inverted`, the "end" is the visual top — the oldest loaded
        // message. RN re-evaluates onEndReached on content-size changes as
        // well as scroll, so a prepend that leaves no scroll offset still
        // advances (desktop PR #958's dead-end shape).
        onEndReached={onEndReached}
        onEndReachedThreshold={0.4}
        onScrollToIndexFailed={onScrollToIndexFailed}
        viewabilityConfigCallbackPairs={viewabilityConfigCallbackPairs}
        ListFooterComponent={
          isFetchingNextPage ? (
            <Text
              style={[ty.meta, { paddingHorizontal: 16, paddingVertical: 10 }]}
            >
              {t("chat.loadingOlder")}
            </Text>
          ) : null
        }
      />
      {sendMessage.isError ? (
        <Text
          accessibilityRole="alert"
          style={[
            ty.meta,
            {
              color: semantic.danger,
              paddingHorizontal: 16,
              paddingTop: 8,
              paddingBottom: 4,
            },
          ]}
        >
          {(sendMessage.error as Error).message || t("chat.sendFailed")}
        </Text>
      ) : null}

      {editTarget ? (
        <EditBar
          draft={editDraft}
          onChangeDraft={setEditDraft}
          onCancel={() => {
            setEditTarget(null);
            setEditDraft("");
          }}
          onSave={onSaveEdit}
          savePending={editMessage.isPending}
        />
      ) : (
        <>
          {replyTarget ? (
            <ReplyBar
              name={
                authorName(replyTarget.sender_id, replyTarget.sender_username, currentUser) ??
                t("chat:list.unknownAuthor")
              }
              snippet={replySnippet(replyTarget) || t("chat:message.encrypted")}
              onPressQuote={() => scrollToMessage(replyTarget.id)}
              onDismiss={() => setReplyTarget(null)}
            />
          ) : null}
          <Composer
            focusRequest={composerFocus}
            draft={draft}
            onChangeDraft={setDraft}
            onSend={onSend}
            sendPending={sendMessage.isPending}
            editable={!!kind && !!conversationId}
            mentionCandidates={mentionCandidates}
            customEmoji={usableEmoji}
            onAttach={() => void onAttach()}
            pendingAttachments={pendingAttachments}
            onRemoveAttachment={(id) =>
              setPendingAttachments((prev) => prev.filter((a) => a.id !== id))
            }
            canSendEmptyText={pendingAttachments.length > 0}
          />
        </>
      )}

      {actionTarget ? (
        <MessageActionsSheet
          target={actionTarget}
          quoteName={
            authorName(
              actionTarget.sender_id,
              actionTarget.sender_username,
              currentUser,
            ) ?? t("chat:list.unknownAuthor")
          }
          quoteTime={timeLabel(actionTarget.created_at)}
          isOwn={actionTarget.sender_id === currentUser?.id}
          isSaved={savedIds.has(actionTarget.id)}
          onToggleSave={() => {
            toggleSaved.mutate(actionTarget.id);
            setActionTarget(null);
          }}
          isPinned={pinnedIds.has(actionTarget.id)}
          onTogglePin={
            conversationId
              ? () => {
                  togglePin.mutate({
                    conversationId,
                    messageId: actionTarget.id,
                    isPinned: pinnedIds.has(actionTarget.id),
                  });
                  setActionTarget(null);
                }
              : undefined
          }
          onCopyText={() => copyToClipboard(actionTarget.content)}
          onCopyLink={() =>
            copyToClipboard(
              formatMessagePermalink(
                actionTarget.conversation_id,
                actionTarget.id,
              ),
            )
          }
          onReact={(emoji) => {
            reactWithEmoji(actionTarget.id, emoji);
            setActionTarget(null);
          }}
          onOpenPicker={() => {
            const m = actionTarget;
            setActionTarget(null);
            // A second sheet (Modal) only after the first is dismissed — iOS
            // refuses to present over a modal that is still going away.
            afterSheetClose(() => setPickerTarget(m));
          }}
          onReply={() => {
            const m = actionTarget;
            setActionTarget(null);
            // Focus only after the sheet's Modal is gone, or iOS drops it.
            afterSheetClose(() => startReply(m));
          }}
          onReplyInThread={() => {
            const rootId = actionTarget.id;
            setActionTarget(null);
            afterSheetClose(() => openThread(rootId));
          }}
          onEdit={() => {
            setReplyTarget(null);
            setEditTarget(actionTarget);
            setEditDraft(actionTarget.content);
            setActionTarget(null);
          }}
          onDelete={() => {
            deleteMessage.mutate(actionTarget.id);
            setActionTarget(null);
          }}
          onReport={() => {
            const m = actionTarget;
            setActionTarget(null);
            afterSheetClose(() =>
              router.push({
                pathname: "/report",
                params: { userId: m.sender_id, conversationId: m.conversation_id, messageId: m.id },
              }),
            );
          }}
          onClose={() => setActionTarget(null)}
        />
      ) : null}

      {pickerTarget ? (
        <EmojiPickerSheet
          onSelect={(emoji) => {
            reactWithEmoji(pickerTarget.id, emoji);
            setPickerTarget(null);
          }}
          onClose={() => setPickerTarget(null)}
        />
      ) : null}

      {menuOpen && conversationId ? (
        <ChannelMenuSheet
          title={title}
          channelId={conversationId}
          onInfo={() => {
            setMenuOpen(false);
            // Navigate once the sheet's Modal is dismissed (see afterSheetClose).
            afterSheetClose(() =>
              router.push({
                pathname: "/conversation/info",
                params: { id: conversationId, kind: "channel" },
              }),
            );
          }}
          onGroupSettings={
            groupId
              ? () => {
                  setMenuOpen(false);
                  afterSheetClose(() =>
                    router.push({
                      pathname: "/group/settings",
                      params: { groupId },
                    }),
                  );
                }
              : undefined
          }
          onClose={() => setMenuOpen(false)}
        />
      ) : null}
    </>
  );

  // Embedded (two-pane right column) sits inside the list screen's own
  // <Screen>/SafeAreaView, so wrap in a plain View to avoid double-insetting.
  // Route usage renders the full <Screen> exactly as before.
  // The embedded view keeps the route's `screen-chat` id so e2e flows find
  // the conversation on both layouts.
  return embedded ? (
    <View testID="screen-chat" style={{ flex: 1, backgroundColor: semantic.bg }}>
      {content}
    </View>
  ) : (
    <Screen testID="screen-chat" wide>
      {content}
    </Screen>
  );
}

export const ChatView = observer(TextChat);

// Regular width (iPad) never shows a conversation full screen: whoever pushed
// this route (a notification, a permalink, an older call site), the
// conversation is selected in its tab and drawn in the two-pane's right pane.
// Call sites use useOpenConversation to go there directly; this is the net
// for anything that still lands here.
function ChatRedirect() {
  const router = useRouter();
  const params = useLocalSearchParams<{ id?: string; kind?: string; name?: string }>();
  const { data: groups, isLoading } = useUserGroupsWithChannels();
  const id = typeof params.id === "string" ? params.id : null;
  const kind = params.kind === "dm" ? "dm" : "channel";
  useEffect(() => {
    if (!id) {
      router.dismissTo("/(tabs)/groups" as Href);
      return;
    }
    // A channel needs its group, which the groups list knows.
    if (kind === "channel" && isLoading) {
      return;
    }
    const groupId =
      kind === "channel"
        ? groups?.find((g) => g.channels.some((c) => c.id === id))?.id ??
          appStore.selectedGroupId
        : null;
    router.dismissTo(selectConversation({ id, kind }, groupId) as Href);
    // Once per opened conversation; the router object is stable enough.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, kind, isLoading]);
  return <Screen testID="screen-chat-redirect" wide>{null}</Screen>;
}

function ChatRoute() {
  const regular = useIsRegular();
  return regular ? <ChatRedirect /> : <ChatView />;
}

export default observer(ChatRoute);
