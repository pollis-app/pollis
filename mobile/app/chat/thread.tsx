import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  View,
  FlatList,
  type NativeScrollEvent,
  type NativeSyntheticEvent,
} from "react-native";
import { Text } from "../../components/Text";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import { Screen, Header, EmptyState } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { GROUP_WINDOW_MS, timeLabel } from "../../components/chat/dates";
import { MessageRow } from "../../components/chat/MessageRow";
import { Composer } from "../../components/chat/Composer";
import { authorName } from "../../lib/authorName";
import { aliasAvatar, aliasName } from "../../lib/groupAlias";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";
import { outgoingText } from "../../lib/messageText";
import { botIds } from "../../lib/bots";
import { useMessageBodyContext } from "../../hooks/useMessageBodyContext";
import {
  useMessages,
  useSendMessage,
  useRetryMessage,
  useDiscardUnsent,
  useThreadMessages,
  useGroupMembers,
  useUserGroupsWithChannels,
  useLinkPreviewsEnabled,
  useIsDmRequest,
  flattenPages,
  type ConversationKind,
  type Message,
} from "../../hooks/queries";
import { retryTarget, unsentReason } from "../../hooks/queries/pendingMessages";
import { useMentionCandidates } from "../../hooks/useMentionCandidates";
import { useOpenMediaViewer } from "../../hooks/useOpenMediaViewer";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useTheme } from "../../components/theme";
import { useDraft } from "../../hooks/useDraft";
import { useLinkPreviewDraft } from "../../hooks/useLinkPreviewDraft";
import { ComposerLinkPreview } from "../../components/chat/ComposerLinkPreview";
import { classifyArrivals, distanceFromBottom, isNearBottom } from "../../lib/chatFollow";
import { useReduceMotion } from "../../hooks/useReduceMotion";
import { REDUCED_FADE_IN, messageRiseIn } from "../../theme/motion";

const RISE = messageRiseIn(false);

/** Slack-style thread screen (#831); state is isolated per thread (#837). */
function ThreadScreen() {
  const { themeKey } = useTheme();
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

  const [draft, setDraft] = useDraft(threadId ? `thread:${threadId}` : null);
  // Link previews (#1987), as in the conversation's composer.
  const { enabled: linkPreviewsEnabled } = useLinkPreviewsEnabled();
  const linkPreview = useLinkPreviewDraft(draft, linkPreviewsEnabled, threadId);
  const listRef = useRef<FlatList<Message>>(null);
  const currentUser = appStore.currentUser;

  // The root comes from the conversation cache; `read_thread_messages` returns only replies.
  const { data: conversationData } = useMessages(conversationId, kind);
  const root = useMemo(
    () => flattenPages(conversationData).find((m) => m.id === threadId) ?? null,
    [conversationData, threadId],
  );

  const { data: replies = [], isLoading } = useThreadMessages(threadId);
  const openMedia = useOpenMediaViewer(conversationId, kind, threadId);
  const sendMessage = useSendMessage(conversationId, kind);
  // `mutate` keeps its identity (#2141): the rows need not re-render on every keystroke.
  const { mutate: retrySend } = useRetryMessage(conversationId, kind, threadId);
  const { mutate: discardSend } = useDiscardUnsent(conversationId, kind, threadId);

  const mentionGroupId =
    kind === "channel" ? appStore.selectedGroupId ?? null : null;
  const mentionCandidates = useMentionCandidates(
    kind,
    conversationId,
    mentionGroupId,
  );
  // Per-group aliases (#2003): the thread's group shows people as it does.
  const aliases = useGroupAliases(mentionGroupId);
  // Bots (#1182): the roster says who is one (the same cached read the mention picker makes).
  const { data: threadMembers } = useGroupMembers(mentionGroupId);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const botGroup = groupsWithChannels?.find((g) => g.id === mentionGroupId)?.name;
  const botSenders = useMemo(() => botIds(threadMembers ?? []), [threadMembers]);
  // Bodies draw as the conversation draws them (#2143): mentions with alias labels, bots, links.
  const {
    mentionNames,
    selfName,
    mentionLabels: aliasMentionLabels,
    botMentions,
  } = useMessageBodyContext(kind, conversationId, mentionGroupId);
  // No sender previews in an unaccepted DM request, as in its conversation (#1987).
  const isPendingRequest = useIsDmRequest(kind, conversationId);

  // The composer hands over its text at the tap (#2099); true = the composer was cleared.
  const onSend = (typed: string): boolean => {
    const text = outgoingText(typed);
    if (!text || !threadId) {
      return false;
    }
    // Never waits for a fetch still running: the reply goes without it.
    const preview = linkPreview.takeForSend(text);
    setDraft("");
    sendMessage.mutate({ content: text, threadId, linkPreview: preview });
    return true;
  };

  // #2125: the same rule as the conversation: follow only from the bottom or after an own reply.
  const reduceMotion = useReduceMotion();
  const reduceMotionRef = useRef(reduceMotion);
  reduceMotionRef.current = reduceMotion;
  const atBottomRef = useRef(true);
  const newestFirst = useMemo(() => replies.map((m) => m.id).reverse(), [replies]);
  const prevIds = useRef<readonly string[] | null>(null);
  const selfId = currentUser?.id;
  const arrivals = useMemo(() => {
    const own = new Set(replies.filter((m) => m.sender_id === selfId).map((m) => m.id));
    return classifyArrivals({
      prev: prevIds.current,
      next: newestFirst,
      atBottom: atBottomRef.current,
      isOwn: (id) => own.has(id),
    });
    // `replies` moves with `newestFirst`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [newestFirst, selfId]);
  const risen = useRef(new Set<string>());
  const rising = useMemo(() => new Set(arrivals.rise), [arrivals]);
  useEffect(() => {
    // The first replies to land open the thread at its newest one.
    const first = prevIds.current === null || prevIds.current.length === 0;
    prevIds.current = newestFirst;
    for (const id of arrivals.rise) {
      risen.current.add(id);
    }
    if ((first && newestFirst.length > 0) || arrivals.follow) {
      requestAnimationFrame(() => {
        listRef.current?.scrollToEnd({ animated: !first && !reduceMotionRef.current });
      });
    }
  }, [arrivals, newestFirst]);
  const onScroll = useCallback((e: NativeSyntheticEvent<NativeScrollEvent>) => {
    const { contentOffset, contentSize, layoutMeasurement } = e.nativeEvent;
    atBottomRef.current = isNearBottom(
      distanceFromBottom(contentOffset.y, false, contentSize.height, layoutMeasurement.height),
    );
  }, []);
  const riseIn = reduceMotion ? REDUCED_FADE_IN : RISE;

  const renderRow = useCallback(
    ({ item: m, index }: { item: Message; index: number }) => {
      const prev = index > 0 ? replies[index - 1] : null;
      const continued =
        !!prev &&
        prev.sender_id === m.sender_id &&
        m.created_at - prev.created_at < GROUP_WINDOW_MS;
      const retryId = retryTarget(m);
      const mine = currentUser?.id === m.sender_id;
      const name =
        aliasName(aliases, m.sender_id, authorName(m.sender_id, m.sender_username, currentUser, m.sender_display_name)) ??
        t("chat:list.unknownAuthor");
      return (
        <MessageRow
          testID={`row-thread-${m.id}`}
          messageId={m.id}
          av={name.slice(0, 2)}
          senderId={m.sender_id}
          senderAliasKey={aliasAvatar(aliases, m.sender_id, null)}
          amber={mine}
          name={name}
          time={timeLabel(m.created_at)}
          text={m.content}
          deleted={!!m.deleted_at}
          attachments={m.attachments}
          linkPreview={isPendingRequest ? null : m.link_preview}
          onOpenAttachment={openMedia}
          pending={m.pending}
          failed={m.failed}
          unsentReason={unsentReason(m)}
          onRetry={retryId ? () => retrySend(retryId) : undefined}
          onDiscard={m.failed ? () => discardSend(m.outboxId ?? m.id) : undefined}
          edited={!!m.edited_at}
          editUnsent={m.edit_unsent}
          continued={continued}
          isBot={botSenders.has(m.sender_id)}
          botGroup={botGroup}
          mentionNames={mentionNames}
          botMentions={botMentions}
          mentionLabels={aliasMentionLabels}
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
          entering={rising.has(m.id) && !risen.current.has(m.id) ? riseIn : undefined}
        />
      );
    },
    [currentUser, router, t, replies, mentionNames, selfName, openMedia, retrySend, discardSend, aliases, botSenders, botGroup, botMentions, aliasMentionLabels, isPendingRequest, rising, riseIn],
  );

  const rootName = root
    ? aliasName(aliases, root.sender_id, authorName(root.sender_id, root.sender_username, currentUser, root.sender_display_name))
    : null;
  const header = (
    <View>
      {root ? (
        <MessageRow
          testID={`row-thread-root-${root.id}`}
          messageId={root.id}
          av={(rootName ?? "??").slice(0, 2)}
          senderId={root.sender_id}
          amber={currentUser?.id === root.sender_id}
          name={rootName ?? t("chat:list.unknownAuthor")}
          time={timeLabel(root.created_at)}
          text={root.content}
          deleted={!!root.deleted_at}
          attachments={root.attachments}
          linkPreview={isPendingRequest ? null : root.link_preview}
          onOpenAttachment={openMedia}
          edited={!!root.edited_at}
          editUnsent={root.edit_unsent}
          isBot={botSenders.has(root.sender_id)}
          botGroup={botGroup}
          mentionNames={mentionNames}
          botMentions={botMentions}
          mentionLabels={aliasMentionLabels}
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
        <EmptyState testID="thread-empty">{t("thread.empty")}</EmptyState>
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
        // As the chat list (#1859): rows, links and tiles answer the first touch with the keyboard up.
        keyboardShouldPersistTaps="handled"
        data={replies}
        keyExtractor={(m) => m.id}
        renderItem={renderRow}
        extraData={themeKey}
        ListHeaderComponent={header}
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingTop: 4, paddingBottom: 12 }}
        onScroll={onScroll}
        scrollEventThrottle={32}
      />
      {/* A failed send is told on its own row (UnsentActions), never as a raw error here. */}
      <Composer
        draft={draft}
        onChangeDraft={setDraft}
        onSend={onSend}
        editable={!!threadId && !!conversationId && !!kind}
        mentionCandidates={mentionCandidates}
        linkPreview={
          <ComposerLinkPreview state={linkPreview.state} onRemove={linkPreview.remove} />
        }
      />
    </Screen>
  );
}

export default observer(ThreadScreen);
