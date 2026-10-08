import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Text, FlatList } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import { Screen, Header } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { GROUP_WINDOW_MS, timeLabel } from "../../components/chat/dates";
import { MessageRow } from "../../components/chat/MessageRow";
import { Composer } from "../../components/chat/Composer";
import { authorName } from "../../lib/authorName";
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
  const router = useNav();
  const { t } = useTranslation("nav");
  const params = useRouteParams<{
    threadId?: string;
    id?: string;
    kind?: string;
    name?: string;
  }>();
  const threadId = params.threadId ?? null;
  const conversationId = params.id ?? null;
  const kind: ConversationKind | null =
    params.kind === "channel" || params.kind === "dm" ? params.kind : null;
  const title =
    typeof params.name === "string" ? params.name : t("panel.thread");

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
    ({ item: m, index }: { item: Message; index: number }) => {
      // Same sender again within the window hangs under the previous reply.
      const prev = index > 0 ? replies[index - 1] : null;
      const continued =
        !!prev &&
        prev.sender_id === m.sender_id &&
        m.created_at - prev.created_at < GROUP_WINDOW_MS;
      const mine = currentUser?.id === m.sender_id;
      const name =
        authorName(m.sender_id, m.sender_username, currentUser) ??
        t("chat:list.unknownAuthor");
      return (
        <MessageRow
          testID={`row-thread-${m.id}`}
          messageId={m.id}
          av={name.slice(0, 2)}
          amber={mine}
          name={name}
          time={timeLabel(m.created_at)}
          text={m.content}
          attachments={m.attachments}
          pending={m.pending}
          failed={m.failed}
          edited={!!m.edited_at}
          continued={continued}
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
    [currentUser, router, t, replies, mentionNames, selfName],
  );

  const rootName = root
    ? authorName(root.sender_id, root.sender_username, currentUser)
    : null;
  const header = (
    <View>
      {root ? (
        <MessageRow
          testID={`row-thread-root-${root.id}`}
          messageId={root.id}
          av={(rootName ?? "??").slice(0, 2)}
          amber={currentUser?.id === root.sender_id}
          name={rootName ?? t("chat:list.unknownAuthor")}
          time={timeLabel(root.created_at)}
          text={root.content}
          attachments={root.attachments}
          edited={!!root.edited_at}
          mentionNames={mentionNames}
          selfName={selfName}
        />
      ) : null}
      <View
        accessibilityRole="header"
        style={{
          flexDirection: "row",
          alignItems: "center",
          gap: 10,
          paddingHorizontal: 16,
          paddingTop: 14,
          paddingBottom: 6,
        }}
      >
        <Text style={[ty.section, { color: semantic.muted }]}>
          {t("chat:thread.replyCount", { count: replies.length })}
        </Text>
        <View style={{ flex: 1, height: 1, backgroundColor: semantic.hair }} />
      </View>
      {isLoading && replies.length === 0 ? (
        <Text
          style={[
            ty.secondary,
            { color: semantic.muted, paddingHorizontal: 16, paddingTop: 4 },
          ]}
        >
          {t("thread.loading")}
        </Text>
      ) : null}
      {!isLoading && replies.length === 0 ? (
        <Text
          style={[
            ty.secondary,
            { color: semantic.muted, paddingHorizontal: 16, paddingTop: 4 },
          ]}
        >
          {t("thread.empty")}
        </Text>
      ) : null}
    </View>
  );

  return (
    <Screen testID="screen-thread" aboveTabBar={router.inPane} wide>
      <Header onBack={router.onBack}
        title={t("panel.thread")}
        subtitle={title !== t("panel.thread") ? title : undefined}
        backTo={title !== t("panel.thread") ? title : undefined}
        titleIcon={<Icon.thread size={15} color={semantic.dim} />}
      />
      <FlatList
        ref={listRef}
        testID="list-thread"
        data={replies}
        keyExtractor={(m) => m.id}
        renderItem={renderRow}
        ListHeaderComponent={header}
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingTop: 4, paddingBottom: 12 }}
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
          {(sendMessage.error as Error).message || t("mobile:thread.sendFailed")}
        </Text>
      ) : null}
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
