import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Text, ScrollView } from "react-native";
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
import {
  useMessages,
  useSendMessage,
  useIngestConversation,
  useToggleReaction,
  useEditMessage,
  useDeleteMessage,
  type ConversationKind,
  type Message,
} from "../../hooks/queries";
import { useConversationRealtime } from "../../hooks/useConversationRealtime";
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
  const [editTarget, setEditTarget] = useState<Message | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const scrollRef = useRef<ScrollView>(null);
  const currentUser = appStore.currentUser;

  const { data, isLoading, isError } = useMessages(conversationId, kind);
  const messages = data?.messages ?? [];
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

  // Auto-scroll to bottom whenever the message list grows (new arrival or
  // optimistic send).
  useEffect(() => {
    if (messages.length === 0) {
      return;
    }
    requestAnimationFrame(() => {
      scrollRef.current?.scrollToEnd({ animated: true });
    });
  }, [messages.length]);

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

  const sections = useMemo(() => {
    const out: { label: string; messages: Message[] }[] = [];
    let lastKey = "";
    for (const m of messages) {
      const k = dayKey(m.created_at);
      if (k !== lastKey) {
        out.push({ label: dayLabel(m.created_at), messages: [] });
        lastKey = k;
      }
      out[out.length - 1].messages.push(m);
    }
    return out;
  }, [messages]);

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

  const content = (
    <>
      <Crumb segs={[{ label: ctxLabel, leaf: true }]} />
      <ScrollView
        ref={scrollRef}
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingVertical: 4 }}
      >
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
        {!isLoading && !isError && sections.length === 0 ? (
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
        {sections.map((section, sIdx) => (
          <View key={`${section.label}-${sIdx}`}>
            <DaySeparator label={section.label} />
            {section.messages.map((m) => {
              const mine = currentUser?.id === m.sender_id;
              const name = m.sender_username || (mine ? "you" : "user");
              return (
                <MessageRow
                  key={m.id}
                  testID={`row-message-${m.id}`}
                  av={name.slice(0, 2)}
                  amber={mine}
                  name={name}
                  time={timeLabel(m.created_at)}
                  text={m.content}
                  pending={m.pending}
                  edited={!!m.edited_at}
                  onPressAvatar={
                    mine
                      ? undefined
                      : () =>
                          router.push({
                            pathname: "/user/[id]",
                            params: { id: m.sender_id },
                          })
                  }
                  onLongPress={
                    m.pending ? undefined : () => setActionTarget(m)
                  }
                />
              );
            })}
          </View>
        ))}
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
      </ScrollView>

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
            toggleReaction.mutate({
              messageId: actionTarget.id,
              emoji,
              mode: "add",
            });
            setActionTarget(null);
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
