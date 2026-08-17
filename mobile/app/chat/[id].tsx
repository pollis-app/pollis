import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Text, FlatList } from "react-native";
import { useFocusEffect, useLocalSearchParams, useRouter } from "expo-router";
import { Screen, Crumb, Ctx, CtxAct } from "../../components/ui";
import { Icon } from "../../components/icons";
import { palette, semantic, type as ty } from "../../theme/tokens";
import { dayKey, dayLabel, timeLabel } from "../../components/chat/dates";
import { DaySeparator } from "../../components/chat/DaySeparator";
import { MessageRow } from "../../components/chat/MessageRow";
import { Composer } from "../../components/chat/Composer";
import { EditBar } from "../../components/chat/EditBar";
import { MessageActionsSheet } from "../../components/chat/MessageActionsSheet";
import { ChannelMenuSheet } from "../../components/chat/ChannelMenuSheet";
import { EmojiPickerSheet } from "../../components/emoji/EmojiPickerSheet";
import {
  useMessages,
  useSendMessage,
  useIngestConversation,
  useToggleReaction,
  useConversationReactions,
  useEditMessage,
  useDeleteMessage,
  useConversationReceipts,
  useSendReadReceipts,
  useThreadSummaries,
  flattenPages,
  type ConversationKind,
  type Message,
} from "../../hooks/queries";
import { useConversationRealtime } from "../../hooks/useConversationRealtime";
import { useReadReceipts } from "../../hooks/useReadReceipts";
import { ensurePushRegistration } from "../../lib/push";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

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
  | { type: "msg"; key: string; message: Message };

function TextChat(props: ChatViewProps = {}) {
  const router = useRouter();
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

  const [draft, setDraft] = useState("");
  const [actionTarget, setActionTarget] = useState<Message | null>(null);
  const [pickerTarget, setPickerTarget] = useState<Message | null>(null);
  const [editTarget, setEditTarget] = useState<Message | null>(null);
  const [editDraft, setEditDraft] = useState("");
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
    () => messages.filter((m) => !m.pending).map((m) => m.id),
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

  const onSend = () => {
    const text = draft.trim();
    if (!text || sendMessage.isPending) {
      return;
    }
    setDraft("");
    sendMessage.mutate({ content: text });
  };

  const onSaveEdit = () => {
    const text = editDraft.trim();
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

  // Inverted-list items: build chronologically (day separator before the
  // first message of each day), then reverse so index 0 is the newest row.
  // Thread replies stay out of the main timeline — they render only in the
  // thread screen (#837's isolation fix; same filter as desktop's
  // `!m.thread_id || m.thread_id === m.id` render boundary).
  const items = useMemo(() => {
    const chrono = [...messages]
      .reverse()
      .filter((m) => !m.thread_id || m.thread_id === m.id);
    const out: ChatListItem[] = [];
    let lastKey = "";
    for (const m of chrono) {
      const k = dayKey(m.created_at);
      if (k !== lastKey) {
        out.push({ type: "sep", key: `sep-${k}`, label: dayLabel(m.created_at) });
        lastKey = k;
      }
      out.push({ type: "msg", key: m.id, message: m });
    }
    out.reverse();
    return out;
  }, [messages]);

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

  const ctxLabel = kind === "dm" ? "DIRECT" : "CHANNEL";

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
    peerName ||
    (kind === "dm" ? "Direct message" : "Channel");

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
      const mine = currentUser?.id === m.sender_id;
      const name = m.sender_username || (mine ? "you" : "user");
      return (
        <MessageRow
          testID={`row-message-${m.id}`}
          messageId={m.id}
          av={name.slice(0, 2)}
          amber={mine}
          name={name}
          time={timeLabel(m.created_at)}
          text={m.content}
          pending={m.pending}
          edited={!!m.edited_at}
          reactions={reactionsByMessage?.get(m.id)}
          currentUserId={currentUser?.id}
          receipt={receiptsByMessage?.get(m.id)}
          peerCount={peerCount}
          showReceipt={mine && isDm}
          threadCount={threadSummaries?.get(m.id)?.reply_count ?? 0}
          onOpenThread={() => openThread(m.id)}
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
          onLongPress={m.pending ? undefined : () => setActionTarget(m)}
        />
      );
    },
    [
      currentUser?.id,
      router,
      reactionsByMessage,
      toggleReaction,
      receiptsByMessage,
      peerCount,
      isDm,
      threadSummaries,
      openThread,
    ],
  );

  const content = (
    <>
      <Crumb segs={[{ label: ctxLabel, leaf: true }]} />
      {isLoading && messages.length === 0 ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 13,
            color: semantic.mute,
            paddingHorizontal: 18,
            paddingTop: 12,
          }}
        >
          Loading messages…
        </Text>
      ) : null}
      {isError ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 13,
            color: semantic.danger,
            paddingHorizontal: 18,
            paddingTop: 12,
          }}
        >
          Couldn't load messages.
        </Text>
      ) : null}
      {!isLoading && !isError && items.length === 0 ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 13,
            color: semantic.mute,
            paddingHorizontal: 18,
            paddingTop: 12,
          }}
        >
          No messages yet. Be the first to say something.
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
        contentContainerStyle={{ paddingVertical: 4 }}
        // With `inverted`, the "end" is the visual top — the oldest loaded
        // message. RN re-evaluates onEndReached on content-size changes as
        // well as scroll, so a prepend that leaves no scroll offset still
        // advances (desktop PR #958's dead-end shape).
        onEndReached={onEndReached}
        onEndReachedThreshold={0.4}
        viewabilityConfigCallbackPairs={viewabilityConfigCallbackPairs}
        ListFooterComponent={
          isFetchingNextPage ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.mute,
                paddingHorizontal: 18,
                paddingVertical: 10,
              }}
            >
              Loading older messages…
            </Text>
          ) : null
        }
      />
      {sendMessage.isError ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 12,
            color: semantic.danger,
            paddingHorizontal: 18,
            paddingTop: 8,
            paddingBottom: 4,
          }}
        >
          {(sendMessage.error as Error).message || "Couldn't send message."}
        </Text>
      ) : null}

      <Ctx
        hideBack={embedded}
        cr={ctxLabel}
        name={
          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 6,
              flex: 1,
              minWidth: 0,
            }}
          >
            {kind === "channel" ? <Icon.hash color={semantic.ink} /> : null}
            <Text
              numberOfLines={1}
              ellipsizeMode="tail"
              style={{
                flex: 1,
                fontFamily: ty.rowN.fontFamily,
                fontSize: 15,
                color: semantic.ink,
              }}
            >
              {title}
            </Text>
          </View>
        }
        actions={
          <>
            <CtxAct
              testID="btn-members"
              accessibilityLabel="Members"
              icon={<Icon.people color={semantic.ink2} />}
              onPress={
                kind === "channel" && groupId
                  ? () =>
                      router.push({
                        pathname: "/group/members",
                        params: { groupId },
                      })
                  : undefined
              }
            />
            <CtxAct
              testID="btn-chat-menu"
              accessibilityLabel="Conversation menu"
              icon={<Icon.kebab color={semantic.ink2} />}
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
        <Composer
          draft={draft}
          onChangeDraft={setDraft}
          onSend={onSend}
          sendPending={sendMessage.isPending}
          editable={!!kind && !!conversationId}
        />
      )}

      {actionTarget ? (
        <MessageActionsSheet
          target={actionTarget}
          isOwn={actionTarget.sender_id === currentUser?.id}
          onReact={(emoji) => {
            reactWithEmoji(actionTarget.id, emoji);
            setActionTarget(null);
          }}
          onOpenPicker={() => {
            setPickerTarget(actionTarget);
            setActionTarget(null);
          }}
          onReplyInThread={() => {
            const rootId = actionTarget.id;
            setActionTarget(null);
            openThread(rootId);
          }}
          onEdit={() => {
            setEditTarget(actionTarget);
            setEditDraft(actionTarget.content);
            setActionTarget(null);
          }}
          onDelete={() => {
            deleteMessage.mutate(actionTarget.id);
            setActionTarget(null);
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
          onInfo={() => {
            setMenuOpen(false);
            router.push({
              pathname: "/conversation/info",
              params: { id: conversationId, kind: "channel" },
            });
          }}
          onGroupSettings={
            groupId
              ? () => {
                  setMenuOpen(false);
                  router.push({
                    pathname: "/group/settings",
                    params: { groupId },
                  });
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
  return embedded ? (
    <View style={{ flex: 1, backgroundColor: palette.bg }}>{content}</View>
  ) : (
    <Screen testID="screen-chat">{content}</Screen>
  );
}

export const ChatView = observer(TextChat);

export default observer(TextChat);
