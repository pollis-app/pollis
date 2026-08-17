import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Text, FlatList } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { Screen, Crumb, Ctx } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { timeLabel } from "../../components/chat/dates";
import { MessageRow } from "../../components/chat/MessageRow";
import { Composer } from "../../components/chat/Composer";
import {
  useMessages,
  useSendMessage,
  useThreadMessages,
  flattenPages,
  type ConversationKind,
  type Message,
} from "../../hooks/queries";
import { useMentionCandidates } from "../../hooks/useMentionCandidates";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

/**
 * Slack-style thread screen (#831): the root message pinned at the top,
 * replies chronologically below, own composer sending with `threadId`.
 * Pushed with its own route state, so thread drafts and lists are isolated
 * per thread and per conversation (#837).
 */
function ThreadScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{
    threadId?: string;
    id?: string;
    kind?: string;
    name?: string;
  }>();
  const threadId = params.threadId ?? null;
  const conversationId = params.id ?? null;
  const kind: ConversationKind | null =
    params.kind === "channel" || params.kind === "dm" ? params.kind : null;
  const title = typeof params.name === "string" ? params.name : "Thread";

  const [draft, setDraft] = useState("");
  const listRef = useRef<FlatList<Message>>(null);
  const currentUser = appStore.currentUser;

  // The conversation cache holds the root (roots are ordinary messages;
  // `read_thread_messages` returns only the replies).
  const { data: conversationData } = useMessages(conversationId, kind);
  const root = useMemo(
    () => flattenPages(conversationData).find((m) => m.id === threadId) ?? null,
    [conversationData, threadId],
  );

  const { data: replies = [], isLoading } = useThreadMessages(threadId);
  const sendMessage = useSendMessage(conversationId, kind);

  // Mentions (#886) — same roster-only pool as the parent conversation.
  // Channels resolve the group from the store (set when the channel opened).
  const mentionGroupId =
    kind === "channel" ? appStore.selectedGroupId ?? null : null;
  const mentionCandidates = useMentionCandidates(
    kind,
    conversationId,
    mentionGroupId,
  );
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

  const onSend = () => {
    const text = draft.trim();
    if (!text || !threadId || sendMessage.isPending) {
      return;
    }
    setDraft("");
    sendMessage.mutate({ content: text, threadId });
  };

  // Keep the newest reply in view as the thread grows.
  const newestId = replies[replies.length - 1]?.id;
  useEffect(() => {
    if (!newestId) {
      return;
    }
    requestAnimationFrame(() => {
      listRef.current?.scrollToEnd({ animated: true });
    });
  }, [newestId]);

  const renderRow = useCallback(
    ({ item: m }: { item: Message }) => {
      const mine = currentUser?.id === m.sender_id;
      const name = m.sender_username || (mine ? "you" : "user");
      return (
        <MessageRow
          testID={`row-thread-${m.id}`}
          messageId={m.id}
          av={name.slice(0, 2)}
          amber={mine}
          name={name}
          time={timeLabel(m.created_at)}
          text={m.content}
          pending={m.pending}
          edited={!!m.edited_at}
          mentionNames={mentionNames}
          selfName={selfName}
          onPressAvatar={
            mine
              ? undefined
              : () =>
                  router.push({
                    pathname: "/user/[id]",
                    params: { id: m.sender_id },
                  })
          }
        />
      );
    },
    [currentUser?.id, router],
  );

  const header = (
    <View>
      {root ? (
        <MessageRow
          testID={`row-thread-root-${root.id}`}
          messageId={root.id}
          av={(root.sender_username || "??").slice(0, 2)}
          amber={currentUser?.id === root.sender_id}
          name={
            root.sender_username ||
            (currentUser?.id === root.sender_id ? "you" : "user")
          }
          time={timeLabel(root.created_at)}
          text={root.content}
          edited={!!root.edited_at}
          mentionNames={mentionNames}
          selfName={selfName}
        />
      ) : null}
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          gap: 10,
          paddingHorizontal: 18,
          paddingVertical: 8,
        }}
      >
        <Text style={[ty.label, { letterSpacing: 2.2 }]}>
          {replies.length === 1 ? "1 REPLY" : `${replies.length} REPLIES`}
        </Text>
        <View
          style={{ flex: 1, height: 1, backgroundColor: semantic.hairSoft }}
        />
      </View>
      {isLoading && replies.length === 0 ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 13,
            color: semantic.mute,
            paddingHorizontal: 18,
            paddingTop: 4,
          }}
        >
          Loading replies…
        </Text>
      ) : null}
      {!isLoading && replies.length === 0 ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 13,
            color: semantic.mute,
            paddingHorizontal: 18,
            paddingTop: 4,
          }}
        >
          No replies yet. Start the thread.
        </Text>
      ) : null}
    </View>
  );

  return (
    <Screen testID="screen-thread">
      <Crumb segs={[{ label: "THREAD", leaf: true }]} />
      <FlatList
        ref={listRef}
        testID="list-thread"
        data={replies}
        keyExtractor={(m) => m.id}
        renderItem={renderRow}
        ListHeaderComponent={header}
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingVertical: 4 }}
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
          {(sendMessage.error as Error).message || "Couldn't send reply."}
        </Text>
      ) : null}
      <Ctx
        cr="THREAD"
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
            <Icon.thread color={semantic.ink} />
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
      />
      <Composer
        draft={draft}
        onChangeDraft={setDraft}
        onSend={onSend}
        sendPending={sendMessage.isPending}
        editable={!!threadId && !!conversationId && !!kind}
        mentionCandidates={mentionCandidates}
      />
    </Screen>
  );
}

export default observer(ThreadScreen);
