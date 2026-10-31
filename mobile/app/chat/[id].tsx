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
import { KeyChangeNoticeRow } from "../../components/chat/KeyChangeNoticeRow";
import { collapseUndecryptable } from "../../lib/collapseUndecryptable";
import { placeKeyChangeNotices } from "../../lib/keyChangeTimeline";
import { useQueryClient } from "@tanstack/react-query";
import { MessageRow } from "../../components/chat/MessageRow";
import { isNewMemberMessage, newMemberJoins } from "../../lib/newMember";
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
  useDMChannel,
  useKeyChangeNotices,
  usePeerVerifications,
  type KeyChangeNotice,
  flattenPages,
  markConversationRead,
  type ConversationKind,
  type Message,
} from "../../hooks/queries";
import { retryTarget } from "../../hooks/queries/pendingMessages";
import { useConversationRealtime } from "../../hooks/useConversationRealtime";
import { useTypingPublisher } from "../../hooks/useTypingPublisher";
import { TypingIndicator } from "../../components/chat/TypingIndicator";
import { useReadReceipts } from "../../hooks/useReadReceipts";
import { useMentionCandidates } from "../../hooks/useMentionCandidates";
import { useUsableEmoji } from "../../hooks/queries/useEmoji";
import * as Clipboard from "expo-clipboard";
import * as ImagePicker from "expo-image-picker";
import { formatMessagePermalink } from "../../lib/permalinks";
import type { PickedAttachment } from "../../lib/attachments";
import { pickedFromAssets } from "../../lib/composerAttachments";
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
import { useTheme, useThemeRerender } from "../../components/theme";

// Props embed this screen in the iPad right pane; route usage passes none.
type ChatViewProps = {
  conversationId?: string | null;
  kind?: ConversationKind | null;
  groupId?: string;
  name?: string;
  embedded?: boolean;
};

type ChatListItem =
  | { type: "sep"; key: string; label: string }
  | { type: "msg"; key: string; message: Message; continued: boolean }
  | { type: "keyChange"; key: string; notice: KeyChangeNotice; at: number };

function TextChat(props: ChatViewProps = {}) {
  const { accentHex } = useTheme();
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
  // The message the composer is replying to (#1270).
  const [replyTarget, setReplyTarget] = useState<Message | null>(null);
  const [composerFocus, setComposerFocus] = useState(0);
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

  // DM receipts (#892), gated on the reciprocal synced preference.
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
  const peerCount = 1;

  // Safety numbers (#1605): durable change notices + this DM's verified state.
  const { data: dmChannel } = useDMChannel(isDm ? conversationId : null);
  const dmPeers = useMemo(
    () => (dmChannel?.members ?? []).filter((m) => m.user_id !== currentUser?.id),
    [dmChannel, currentUser?.id],
  );
  const dmPeerIds = useMemo(() => dmPeers.map((p) => p.user_id), [dmPeers]);
  const { data: keyChangeNotices } = useKeyChangeNotices(dmPeerIds);
  const { data: peerVerifications } = usePeerVerifications();
  const queryClient = useQueryClient();
  // A new notice means a key was just re-pinned and `verified` cleared: refresh the badge.
  const newestNoticeId = keyChangeNotices?.length
    ? keyChangeNotices[keyChangeNotices.length - 1].id
    : null;
  useEffect(() => {
    if (newestNoticeId === null) {
      return;
    }
    queryClient.invalidateQueries({ queryKey: ["safety", "list"] });
  }, [newestNoticeId, queryClient]);
  const onePeer = dmPeers.length === 1 ? dmPeers[0] : null;
  const peerVerified =
    !!onePeer &&
    (peerVerifications ?? []).some(
      (v) => v.peer_user_id === onePeer.user_id && v.verified,
    );
  const peerLabel = useCallback(
    (userId: string) => {
      const p = dmPeers.find((m) => m.user_id === userId);
      return p?.username ? `@${p.username}` : undefined;
    },
    [dmPeers],
  );
  const sendMessage = useSendMessage(conversationId, kind);
  const retryMessage = useRetryMessage(conversationId, kind);
  const discardUnsent = useDiscardUnsent(conversationId, kind);
  const ingest = useIngestConversation();
  const toggleReaction = useToggleReaction(conversationId, kind);
  const editMessage = useEditMessage(conversationId, kind);
  const deleteMessage = useDeleteMessage(conversationId, kind);

  // Foreground realtime; the group room is named by group_id, a DM's by its id.
  const groupId =
    props.groupId ??
    (kind === "channel" ? appStore.selectedGroupId ?? undefined : undefined);
  useConversationRealtime(conversationId, kind, groupId);
  const typing = useTypingPublisher(conversationId, kind, groupId);
  const onChangeDraft = useCallback(
    (text: string) => {
      setDraft(text);
      typing.notify(text);
    },
    [typing],
  );

  // Contextual notification permission: asked on first conversation open, never at login.
  useEffect(() => {
    const uid = currentUser?.id;
    if (!uid) {
      return;
    }
    void ensurePushRegistration(uid);
  }, [currentUser?.id]);

  useFocusEffect(
    useCallback(() => {
      if (conversationId && kind) {
        void ingest(conversationId, kind);
      }
    }, [conversationId, kind, ingest]),
  );

  // Unread (#1422): on screen it never badges; the cursor advances on arrival and on leaving.
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

  // Keyed on the newest id, so loading an older page never scrolls the reader.
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

  useEffect(() => {
    if (!conversationId || !newestId) {
      return;
    }
    if (appStore.openConversationId !== conversationId) {
      return;
    }
    markConversationRead(conversationId);
  }, [conversationId, newestId]);

  // The iPad pane stays mounted across conversations: a reply must not outlive its own.
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
    if (!text && pendingAttachments.length === 0) {
      return;
    }
    setDraft("");
    typing.stop();
    setPendingAttachments([]);
    setReplyTarget(null);
    sendMessage.mutate({
      content: text,
      attachments: pendingAttachments,
      replyToId: replyTarget?.id,
    });
  };

  const onAttach = useCallback(async () => {
    const result = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: ["images", "videos"],
      allowsMultipleSelection: true,
      quality: 1,
    });
    if (result.canceled) {
      return;
    }
    const picked = pickedFromAssets(result.assets, Date.now());
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

  const { data: threadSummaries } = useThreadSummaries(conversationId);

  // Mention candidates come from the visible roster only (#886).
  const { data: usableEmoji } = useUsableEmoji();
  const mentionCandidates = useMentionCandidates(
    kind,
    conversationId,
    groupId ?? null,
  );
  const savedIds = useSavedMessageIds();
  const toggleSaved = useToggleSavedMessage();
  const pinnedIds = usePinnedMessageIds(conversationId);
  const togglePin = useTogglePinnedMessage();

  // #897: copy is VERIFIED — a thrown write reads as failure.
  const copyToClipboard = useCallback(async (text: string) => {
    let copied = false;
    try {
      copied = await Clipboard.setStringAsync(text);
    } catch {
      copied = false;
    }
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

  // Inverted-list items, newest first. Thread replies stay out of the main timeline (#837).
  const items = useMemo(() => {
    const chrono = collapseUndecryptable(
      [...messages].reverse().filter((m) => !m.thread_id || m.thread_id === m.id),
    );
    const out: ChatListItem[] = [];
    let lastKey = "";
    let prev: Message | null = null;
    // Safety-number changes (#1605) at the time they were detected; a system line breaks a group.
    const placed = placeKeyChangeNotices(
      keyChangeNotices ?? [],
      chrono.length ? chrono[0].created_at : null,
      chrono.length ? chrono[chrono.length - 1].created_at : null,
      !!hasNextPage,
      false,
    );
    let next = 0;
    const emitNotice = (n: (typeof placed)[number]) => {
      const k = dayKey(n.at);
      if (k !== lastKey) {
        out.push({ type: "sep", key: `sep-${k}`, label: dayLabel(n.at) });
        lastKey = k;
      }
      out.push({
        type: "keyChange",
        key: `key-change-${n.notice.id}`,
        notice: n.notice,
        at: n.at,
      });
      prev = null;
    };
    for (const m of chrono) {
      while (next < placed.length && placed[next].at < m.created_at) {
        emitNotice(placed[next]);
        next += 1;
      }
      const k = dayKey(m.created_at);
      if (k !== lastKey) {
        out.push({ type: "sep", key: `sep-${k}`, label: dayLabel(m.created_at) });
        lastKey = k;
        prev = null;
      }
      // Discord-style grouping; a reply always starts its own group.
      const continued =
        prev !== null &&
        !m.reply_to_id &&
        prev.sender_id === m.sender_id &&
        m.created_at - prev.created_at < GROUP_WINDOW_MS;
      out.push({ type: "msg", key: m.id, message: m, continued });
      prev = m;
    }
    while (next < placed.length) {
      emitNotice(placed[next]);
      next += 1;
    }
    out.reverse();
    return out;
  }, [messages, keyChangeNotices, hasNextPage]);

  const repliedTo = useMemo(() => replyTargets(messages), [messages]);

  // Only loaded messages are reachable; an unloaded quote is not pressable.
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
  // Unmeasured rows: get near by average height, then aim again.
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

  // A jump queued by pins, Saved or a permalink: page back until rendered, then claim it.
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
      // Next frame: a just-rendered row has no layout yet.
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

  // The two-pane layout embeds this view without a channel name.
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
  // The "new" member tag (#1506), same rule as desktop (`lib/newMember.ts`).
  const newMemberJoinMs = useMemo(
    () => newMemberJoins(groupMembers ?? [], group?.owner_id),
    [groupMembers, group?.owner_id],
  );

  // Header title: never the raw conversation ULID.
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
      if (item.type === "keyChange") {
        const label = peerLabel(item.notice.peer_user_id);
        return (
          <KeyChangeNoticeRow
            noticeId={item.notice.id}
            peerLabel={label}
            time={timeLabel(item.at)}
            onVerify={() =>
              router.push({
                pathname: "/dm/safety",
                params: {
                  peerId: item.notice.peer_user_id,
                  ...(label ? { name: label } : {}),
                },
              })
            }
          />
        );
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
          editUnsent={m.edit_unsent}
          reactions={reactionsByMessage?.get(m.id)}
          currentUserId={currentUser?.id}
          receipt={receiptsByMessage?.get(m.id)}
          peerCount={peerCount}
          showReceipt={mine && isDm}
          continued={item.continued}
          isNew={isNewMemberMessage(newMemberJoinMs, m.sender_id, m.created_at)}
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
      newMemberJoinMs,
      peerLabel,
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
          ) : peerVerified ? (
            // Only while `verified = 1`: a key change clears it and the timeline says why (#1605).
            <View
              testID="icon-dm-verified"
              accessible
              accessibilityLabel={t("dms:list.verified")}
            >
              <Icon.shieldCheck size={15} color={semantic.accent} />
            </View>
          ) : undefined
        }
        // Each action only opens a page or sheet: touch-down (#1529).
        actions={
          <>
            <IconButton
              testID="btn-pins"
              pressIn
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
              pressIn
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
              pressIn
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
        extraData={accentHex}
        style={{ flex: 1 }}
        // `flex-end` is the visual TOP under `inverted`: anchors a short thread there.
        contentContainerStyle={{
          paddingTop: 4,
          paddingBottom: 12,
          flexGrow: 1,
          justifyContent: "flex-end",
        }}
        // With `inverted`, the "end" is the visual top (the oldest loaded message).
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
          <TypingIndicator
            channelId={kind === "channel" ? conversationId : null}
            conversationId={kind === "dm" ? conversationId : null}
          />
          <Composer
            focusRequest={composerFocus}
            draft={draft}
            onChangeDraft={onChangeDraft}
            onSend={onSend}
            editable={!!kind && !!conversationId}
            mentionCandidates={mentionCandidates}
            customEmoji={usableEmoji}
            onAttach={() => void onAttach()}
            pendingAttachments={pendingAttachments}
            onRemoveAttachment={(id) =>
              setPendingAttachments((prev) => prev.filter((a) => a.id !== id))
            }
            onChangeAttachment={(id, patch) =>
              setPendingAttachments((prev) => prev.map((a) => (a.id === id ? { ...a, ...patch } : a)))
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
            // iOS refuses to present a Modal over one still going away.
            afterSheetClose(() => setPickerTarget(m));
          }}
          onReply={() => {
            const m = actionTarget;
            setActionTarget(null);
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

  // Embedded: a plain View avoids double-insetting; it keeps the `screen-chat` id for e2e.
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

// Regular width never shows a conversation full screen: select it in its tab instead.
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
    if (kind === "channel" && isLoading) {
      return;
    }
    const groupId =
      kind === "channel"
        ? groups?.find((g) => g.channels.some((c) => c.id === id))?.id ??
          appStore.selectedGroupId
        : null;
    router.dismissTo(selectConversation({ id, kind }, groupId) as Href);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, kind, isLoading]);
  return <Screen testID="screen-chat-redirect" wide>{null}</Screen>;
}

function ChatRoute() {
  useThemeRerender();
  const regular = useIsRegular();
  return regular ? <ChatRedirect /> : <ChatView />;
}

export default observer(ChatRoute);
