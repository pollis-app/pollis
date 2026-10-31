import React, { useCallback, useRef, useMemo, useState, useEffect } from "react";
import { Trans, useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { observer } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { MessageList } from "../Message/MessageList";
import { ReplyPreview } from "../Message/ReplyPreview";
import { MessageQueue } from "../Message/MessageQueue";
import { ChatInput, type Attachment, type ChatInputHandle } from "../ui/ChatInput";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { Button } from "../ui/Button";
import { EditMessageBar } from "../ui/EditMessageBar";
import { useMessages, useSendMessage, messageQueryKeys, useDeleteMessage, useEditMessage, useAcceptDMRequest, useBlockUser } from "../../hooks/queries";
import { transformChannelMessage, type RawChannelMessage, useThreadSummaries, takeSendTurn } from "../../hooks/queries/useMessages";
import { useRightPanel } from "./RightPanel/useRightPanel";
import { useGroupMembers, useDeleteChannel } from "../../hooks/queries/useGroups";
import type { Message, MessageAttachment } from "../../types";
import { outgoingAttachments, releaseStaged, type OutgoingAttachment } from "../../utils/attachmentEnvelope";
import { useTypingPublisher } from "../../hooks/useTypingPublisher";
import { messageNavStore } from "../../stores/messageNavStore";
import { messageJumpStore } from "../../stores/messageJumpStore";
import { TypingIndicator } from "../TypingIndicator";
import { EmptyState } from "../ui/EmptyState";
import { probeRender } from "../../utils/renderProbe";

// DM not yet accepted: the chat input becomes an accept/block bar.
export interface PendingDmRequest {
  senderUserId: string;
  senderName: string;
  onAccepted?: () => void;
  onBlocked?: () => void;
}

type PageCursor = { sent_at: string; id: string };

type RawMessagePage = {
  messages: RawChannelMessage[];
  next_cursor: PageCursor | null;
};

type MessagesQueryData = {
  messages: Message[];
  nextCursor: PageCursor | null;
};

/** Pages fetched beyond the live page (#1039). Live: older pages behind the live tail. Anchored: a window around a jump target, paged both ways, with the live page hidden until contiguous. Stamped with its log key (#927). */
type MessageWindow =
  | {
      mode: "live";
      logKey: string;
      messages: Message[];
      cursor: PageCursor | null;
      /** Before the first fetch, continue from the initial page's cursor. */
      fetched: boolean;
    }
  | {
      mode: "anchored";
      logKey: string;
      messages: Message[];
      olderCursor: PageCursor | null;
      newerCursor: PageCursor;
    };

const NO_WINDOW: MessageWindow = {
  mode: "live",
  logKey: "",
  messages: [],
  cursor: null,
  fetched: false,
};

const PAGE_SIZE = 50;

function overlaps(a: Message[], ids: Set<string>): boolean {
  return a.some((m) => ids.has(m.id));
}

/** `sent_at`, then `id`: the page reads' tie-break. */
function isOlder(a: PageCursor, b: PageCursor): boolean {
  return a.sent_at < b.sent_at || (a.sent_at === b.sent_at && a.id < b.id);
}

function olderOf(a: PageCursor, b: PageCursor): PageCursor {
  return isOlder(a, b) ? a : b;
}

function newerOf(a: PageCursor, b: PageCursor): PageCursor {
  return isOlder(a, b) ? b : a;
}

interface MainContentProps {
  pendingDmRequest?: PendingDmRequest | null;
}

export const MainContent: React.FC<MainContentProps> = observer(({ pendingDmRequest = null }) => {
  probeRender("MainContent");
  const { t } = useTranslation("nav");
  const {
    selectedChannelId,
    selectedConversationId,
    selectedGroupId,
    replyToMessageId,
    setReplyToMessageId,
    currentUser,
    pendingDeleteChannelId,
    setPendingDeleteChannelId,
  } = appStore;
  const acceptDmRequestMutation = useAcceptDMRequest();
  const blockUserMutation = useBlockUser();
  const navigate = useNavigate();
  const routeSearch = useSearch({ strict: false }) as { message?: string };
  const deleteChannelMutation = useDeleteChannel();
  const isDeletingThisChannel =
    !!selectedChannelId && pendingDeleteChannelId === selectedChannelId;

  const { data: groupMembers = [] } = useGroupMembers(selectedGroupId ?? null);
  const adminUserIds = useMemo(
    () => new Set(groupMembers.filter((m) => m.role === "admin").map((m) => m.user_id)),
    [groupMembers],
  );
  const viewerIsAdmin =
    !!selectedGroupId && !!currentUser && adminUserIds.has(currentUser.id);

  const chatInputRef = useRef<ChatInputHandle>(null);

  // LiveKit room: the group's MLS id for channels, the conversation id for DMs.
  const typingRoomId = selectedChannelId
    ? selectedGroupId ?? null
    : selectedConversationId ?? null;
  const typing = useTypingPublisher({
    roomId: typingRoomId,
    channelId: selectedChannelId ?? null,
    conversationId: selectedChannelId ? null : selectedConversationId ?? null,
  });

  const queryClient = useQueryClient();
  const { messages, nextCursor, isLoading: messagesLoading } = useMessages(
    selectedChannelId,
    selectedConversationId
  );
  const sendMessageMutation = useSendMessage();
  const { openThread, setSafety } = useRightPanel();
  const { data: threadSummaries } = useThreadSummaries(
    selectedChannelId ?? selectedConversationId ?? null,
  );
  const threadReplyCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const [threadId, summary] of threadSummaries ?? []) {
      counts.set(threadId, summary.reply_count);
    }
    return counts;
  }, [threadSummaries]);
  const deleteMessageMutation = useDeleteMessage();
  const editMessageMutation = useEditMessage();

  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  const [editingMessage, setEditingMessage] = useState<Message | null>(null);
  const [editDraftValue, setEditDraftValue] = useState('');

  const logKey = `${selectedChannelId ?? ""}|${selectedConversationId ?? ""}`;

  const [messageWindow, setMessageWindow] = useState<MessageWindow>(NO_WINDOW);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadingNewer, setLoadingNewer] = useState(false);
  // Bumped when an anchored window is dropped wholesale (a send), so the log re-opens at newest.
  const [windowEpoch, setWindowEpoch] = useState(0);

  // Another conversation's pages are simply not read.
  const window_ = messageWindow.logKey === logKey ? messageWindow : NO_WINDOW;
  const isAnchored = window_.mode === "anchored";
  const windowMessages = window_.messages;
  // Next OLDER cursor, derived during render: an effect read the previous conversation's pages (#927).
  const pageCursor =
    window_.mode === "anchored"
      ? window_.olderCursor
      : window_.fetched
        ? window_.cursor
        : nextCursor;
  const newerCursor = window_.mode === "anchored" ? window_.newerCursor : null;

  useEffect(() => {
    setEditingMessage(null);
  }, [selectedChannelId, selectedConversationId]);

  // Capture phase so AppShell's navigation handler doesn't fire first.
  useEffect(() => {
    if (!editingMessage && !pendingDeleteId && !replyToMessageId && !isDeletingThisChannel) {
      return;
    }
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopImmediatePropagation();
        if (editingMessage) {
          handleCancelEdit();
        } else if (pendingDeleteId) {
          setPendingDeleteId(null);
        } else if (isDeletingThisChannel) {
          setPendingDeleteChannelId(null);
        } else {
          setReplyToMessageId(null);
        }
      }
    };
    window.addEventListener('keydown', handler, { capture: true });
    return () => window.removeEventListener('keydown', handler, { capture: true });
  }, [editingMessage, pendingDeleteId, replyToMessageId, isDeletingThisChannel]);

  useEffect(() => {
    const handler = () => chatInputRef.current?.focus();
    window.addEventListener("pollis:focus-chat-input", handler);
    return () => window.removeEventListener("pollis:focus-chat-input", handler);
  }, []);

  // Live: older pages + live page, deduped and sorted. Anchored: the window alone (#1039).
  const allMessages = useMemo(() => {
    const combined = isAnchored ? windowMessages : [...windowMessages, ...messages];
    const seen = new Set<string>();
    const deduped: Message[] = [];
    for (const m of combined) {
      if (!seen.has(m.id)) {
        seen.add(m.id);
        deduped.push(m);
      }
    }
    return deduped.sort((a, b) => a.created_at - b.created_at);
  }, [isAnchored, windowMessages, messages]);

  // Thread replies stay out of the channel (#825); `allMessages` still resolves them for edit/delete and ReplyPreview.
  const channelMessages = useMemo(
    () => allMessages.filter((m) => !m.thread_id || m.thread_id === m.id),
    [allMessages],
  );

  // Ref: a fresh callback re-renders the whole list on every edit keystroke (#874).
  const allMessagesRef = useRef(allMessages);
  allMessagesRef.current = allMessages;

  const loadMore = useCallback(async () => {
    if (!pageCursor || loadingMore || !currentUser) {
      return;
    }
    setLoadingMore(true);
    let prepended = false;
    try {
      let page: RawMessagePage;
      if (selectedChannelId) {
        page = await invoke<RawMessagePage>('read_channel_messages', {
          channelId: selectedChannelId,
          limit: 50,
          cursor: pageCursor,
        });
      } else if (selectedConversationId) {
        page = await invoke<RawMessagePage>('read_dm_messages', {
          dmChannelId: selectedConversationId,
          limit: 50,
          cursor: pageCursor,
        });
      } else {
        return;
      }

      const fetched = page.messages.map(transformChannelMessage);
      const continuation = page.next_cursor ?? null;
      prepended = true;

      // Keyed to the log it was issued for, so a mid-flight switch can't cross-prepend.
      setMessageWindow((prev) => {
        const base = prev.logKey === logKey ? prev : NO_WINDOW;
        const existingIds = new Set(base.messages.map((m) => m.id));
        const newOnes = fetched.filter((m) => !existingIds.has(m.id));
        const merged = [...newOnes, ...base.messages];
        if (base.mode === "anchored") {
          return { ...base, logKey, messages: merged, olderCursor: continuation };
        }
        return {
          mode: "live",
          logKey,
          messages: merged,
          cursor: continuation,
          fetched: true,
        };
      });
    } finally {
      // Only paths that never prepended clear the flag here; a successful load clears it a commit later (#934).
      if (!prepended) {
        setLoadingMore(false);
      }
    }
  }, [pageCursor, loadingMore, currentUser, logKey, selectedChannelId, selectedConversationId]);

  // Passive effect: runs after the commit with the new rows, so `loadingMore` spans that commit.
  useEffect(() => {
    setLoadingMore(false);
    setLoadingNewer(false);
  }, [messageWindow]);

  const liveMessagesRef = useRef(messages);
  liveMessagesRef.current = messages;
  const windowRef = useRef(window_);
  windowRef.current = window_;

  // Anchored window: page toward the present (#1039); hand back to live on overlap, a short page, or a read failure.
  const loadNewer = useCallback(async () => {
    if (!newerCursor || loadingNewer) {
      return;
    }
    const conversationId = selectedChannelId ?? selectedConversationId;
    if (!conversationId) {
      return;
    }
    setLoadingNewer(true);
    let page: RawMessagePage | null = null;
    try {
      page = await invoke<RawMessagePage>("read_messages_after", {
        conversationId,
        cursor: newerCursor,
        limit: PAGE_SIZE,
      });
    } catch (error) {
      console.error("Failed to load newer messages:", error);
    }
    const base = windowRef.current;
    if (base.logKey !== logKey || base.mode !== "anchored") {
      // Already handed back; nothing to append.
      setLoadingNewer(false);
      return;
    }
    const fetched = page ? page.messages.map(transformChannelMessage) : [];
    const continuation = page?.next_cursor ?? null;
    const liveIds = new Set(liveMessagesRef.current.map((m) => m.id));
    const rejoin = !continuation || overlaps(fetched, liveIds);
    const existingIds = new Set(base.messages.map((m) => m.id));
    const merged = [...base.messages, ...fetched.filter((m) => !existingIds.has(m.id))];
    // The window effect clears the flag a commit later (#934).
    setMessageWindow(
      rejoin
        ? { mode: "live", logKey, messages: merged, cursor: base.olderCursor, fetched: true }
        : { ...base, messages: merged, newerCursor: continuation },
    );
  }, [newerCursor, loadingNewer, logKey, selectedChannelId, selectedConversationId]);

  // A send from an anchored window returns to the present; its pages are dropped (not contiguous).
  const leaveAnchoredWindow = useCallback(() => {
    setMessageWindow((prev) => {
      if (prev.logKey !== logKey || prev.mode !== "anchored") {
        return prev;
      }
      return NO_WINDOW;
    });
    setWindowEpoch((n) => n + 1);
  }, [logKey]);

  // Jump to an unloaded message (#850, #1039): merge if contiguous, else open an anchored window.
  const ensureMessageLoaded = useCallback(
    async (messageId: string) => {
      if (allMessagesRef.current.some((m) => m.id === messageId)) {
        return;
      }
      const conversationId = selectedChannelId ?? selectedConversationId;
      if (!conversationId) {
        return;
      }
      const page = await invoke<RawMessagePage>("read_messages_around", {
        conversationId,
        messageId,
        limit: PAGE_SIZE,
      });
      if (page.messages.length === 0) {
        return;
      }
      const fetched = page.messages.map(transformChannelMessage);
      const newest = page.messages[0];
      const pageNewer: PageCursor = { sent_at: newest.sent_at, id: newest.id };
      const pageOlder = page.next_cursor ?? null;
      const liveIds = new Set(liveMessagesRef.current.map((m) => m.id));
      const contiguousWithLive = overlaps(fetched, liveIds);
      const base = windowRef.current;
      const baseIds = new Set(base.messages.map((m) => m.id));
      const touchesBase = base.messages.length > 0 && overlaps(fetched, baseIds);
      const newOnes = fetched.filter((m) => !baseIds.has(m.id));

      if (base.mode === "live") {
        if (contiguousWithLive || touchesBase) {
          // A page touching the live log continues "load more" from its tail.
          setMessageWindow({
            mode: "live",
            logKey,
            messages: [...newOnes, ...base.messages],
            cursor: pageOlder,
            fetched: true,
          });
          return;
        }
        setMessageWindow({
          mode: "anchored",
          logKey,
          messages: fetched,
          olderCursor: pageOlder,
          newerCursor: pageNewer,
        });
        return;
      }

      if (touchesBase) {
        // Overlapping jump: one window spanning both edges.
        const merged = [...newOnes, ...base.messages];
        const olderCursor =
          pageOlder && base.olderCursor
            ? olderOf(pageOlder, base.olderCursor)
            : null;
        setMessageWindow(
          contiguousWithLive
            ? { mode: "live", logKey, messages: merged, cursor: olderCursor, fetched: true }
            : {
                mode: "anchored",
                logKey,
                messages: merged,
                olderCursor,
                newerCursor: newerOf(pageNewer, base.newerCursor),
              },
        );
        return;
      }

      // A jump clean out of the window drops it (not contiguous).
      setMessageWindow(
        contiguousWithLive
          ? { mode: "live", logKey, messages: fetched, cursor: pageOlder, fetched: true }
          : { mode: "anchored", logKey, messages: fetched, olderCursor: pageOlder, newerCursor: pageNewer },
      );
    },
    [logKey, selectedChannelId, selectedConversationId],
  );

  // Best-effort; also queued on the store so the row flashes once it exists.
  const handleScrollToMessage = useCallback(
    (messageId: string) => {
      const conversationId = selectedChannelId ?? selectedConversationId ?? "";
      messageJumpStore.request(conversationId, messageId);
      void ensureMessageLoaded(messageId).catch((error) => {
        console.error("Failed to load the page around a message:", error);
      });
    },
    [ensureMessageLoaded, selectedChannelId, selectedConversationId],
  );

  // A queued jump this log doesn't hold (#1039): fetch around it, once per request.
  const pendingJumpMessageId = messageJumpStore.messageId;
  const pendingJumpConversationId = messageJumpStore.conversationId;
  const fetchedJumpRef = useRef<string | null>(null);
  useEffect(() => {
    const conversationId = selectedChannelId ?? selectedConversationId;
    // Wait for the live page: needed to decide whether a window is needed.
    if (
      !pendingJumpMessageId ||
      !conversationId ||
      messagesLoading ||
      pendingJumpConversationId !== conversationId ||
      fetchedJumpRef.current === pendingJumpMessageId ||
      allMessagesRef.current.some((m) => m.id === pendingJumpMessageId)
    ) {
      return;
    }
    fetchedJumpRef.current = pendingJumpMessageId;
    void ensureMessageLoaded(pendingJumpMessageId).catch((error) => {
      console.error("Failed to load the page around a message:", error);
    });
  }, [
    pendingJumpMessageId,
    pendingJumpConversationId,
    messagesLoading,
    selectedChannelId,
    selectedConversationId,
    ensureMessageLoaded,
  ]);

  // `?message=` survives reloads; consumed once, then stripped.
  const jumpMessageId = routeSearch.message;
  useEffect(() => {
    if (!jumpMessageId) {
      return;
    }
    messageJumpStore.request(
      selectedChannelId ?? selectedConversationId ?? "",
      jumpMessageId,
    );
    handleScrollToMessage(jumpMessageId);
    void navigate({
      to: ".",
      search: (prev: Record<string, unknown>) => ({ ...prev, message: undefined }),
      replace: true,
    });
  }, [jumpMessageId, selectedChannelId, selectedConversationId, handleScrollToMessage, navigate]);

  const handleConfirmDelete = async () => {
    if (!pendingDeleteId) {
      return;
    }
    try {
      await deleteMessageMutation.mutateAsync({ messageId: pendingDeleteId });
    } catch (error) {
      console.error("Failed to delete message:", error);
    } finally {
      setPendingDeleteId(null);
    }
  };

  const handleConfirmDeleteChannel = async () => {
    if (!selectedChannelId || !selectedGroupId) {
      return;
    }
    try {
      await deleteChannelMutation.mutateAsync({
        groupId: selectedGroupId,
        channelId: selectedChannelId,
      });
      setPendingDeleteChannelId(null);
      navigate({ to: "/groups/$groupId", params: { groupId: selectedGroupId } });
    } catch (error) {
      console.error("Failed to delete channel:", error);
    }
  };

  // Pinned: this re-renders on every edit keystroke (#874).
  const handleEdit = useCallback((messageId: string) => {
    const message = allMessagesRef.current.find((m) => m.id === messageId);
    if (!message) {
      return;
    }
    setPendingDeleteId(null);
    setReplyToMessageId(null);
    setEditDraftValue(message.content_decrypted ?? '');
    setEditingMessage(message);
  }, [setReplyToMessageId]);

  const handleCancelEdit = () => {
    setEditingMessage(null);
  };

  const handleDelete = useCallback((messageId: string) => {
    setEditingMessage(null);
    setReplyToMessageId(null);
    setPendingDeleteId(messageId);
  }, [setReplyToMessageId]);

  const handleReply = useCallback((id: string) => {
    setEditingMessage(null);
    setPendingDeleteId(null);
    setReplyToMessageId(id);
    chatInputRef.current?.focus();
  }, [setReplyToMessageId]);

  const focusComposer = useCallback(() => {
    chatInputRef.current?.focus();
  }, []);

  const getAuthorUsername = useCallback(
    (authorId: string, message?: Message) =>
      message?.sender_username ||
      (authorId === currentUser?.id ? (currentUser?.username ?? authorId) : authorId),
    [currentUser?.id, currentUser?.username],
  );

  const handleSaveEdit = async () => {
    const trimmed = editDraftValue.trim();
    if (!trimmed || !editingMessage) {
      return;
    }
    const conversationId = selectedChannelId ?? selectedConversationId;
    if (!conversationId) {
      return;
    }
    try {
      await editMessageMutation.mutateAsync({
        conversationId,
        channelId: selectedChannelId ?? undefined,
        messageId: editingMessage.id,
        newContent: trimmed,
      });
      setEditingMessage(null);
    } catch (error) {
      console.error("Failed to edit message:", error);
    }
  };

  const handleAcceptDmRequest = async () => {
    if (!pendingDmRequest || !selectedConversationId) {
      return;
    }
    try {
      await acceptDmRequestMutation.mutateAsync(selectedConversationId);
      pendingDmRequest.onAccepted?.();
    } catch (err) {
      console.error("Failed to accept DM request:", err);
    }
  };

  const handleBlockDmRequest = async () => {
    if (!pendingDmRequest) {
      return;
    }
    try {
      await blockUserMutation.mutateAsync(pendingDmRequest.senderUserId);
      pendingDmRequest.onBlocked?.();
    } catch (err) {
      console.error("Failed to block user:", err);
    }
  };

  const handleSend = async (text: string, attachments: Attachment[]) => {
    if (!text.trim() && attachments.length === 0) {
      return;
    }
    if (!selectedChannelId && !selectedConversationId) {
      return;
    }
    if (!currentUser) {
      return;
    }
    // The optimistic insert lands on the live page (#1039).
    if (isAnchored) {
      leaveAnchoredWindow();
    }

    const contentText = text.trim();
    const queryKey = selectedChannelId
      ? messageQueryKeys.channel(selectedChannelId)
      : messageQueryKeys.conversation(selectedConversationId!);

    const optimisticAttachments: MessageAttachment[] = attachments.map((att) => ({
      id: att.id,
      object_key: '',
      content_hash: '',
      sha256: '',
      key_scope: null,
      filename: att.name,
      content_type: att.mimeType,
      file_size: att.size,
      uploaded_at: Date.now(),
      localPreviewUrl: att.preview,
      blurhash: att.blurhash,
      sensitive: att.sensitive,
    }));

    const optimisticId = `pending-${Date.now()}-${Math.random()}`;
    const optimisticMessage: Message = {
      id: optimisticId,
      channel_id: selectedChannelId ?? undefined,
      conversation_id: selectedConversationId ?? undefined,
      sender_id: currentUser.id,
      sender_username: currentUser.username ?? undefined,
      ciphertext: new Uint8Array(),
      nonce: new Uint8Array(),
      content_decrypted: contentText,
      attachments: optimisticAttachments.length > 0 ? optimisticAttachments : undefined,
      is_pinned: false,
      created_at: Date.now(),
      delivered: false,
      status: 'sending',
      reply_to_message_id: replyToMessageId ?? undefined,
    };

    queryClient.setQueryData<MessagesQueryData>(queryKey, (old) => {
      const prev = old ?? { messages: [], nextCursor: null };
      return { ...prev, messages: [...prev.messages, optimisticMessage] };
    });
    setReplyToMessageId(null);
    // Taken now, before any await, so rapid sends leave in the order they were made (#1636).
    const turn = takeSendTurn(selectedChannelId || selectedConversationId!);

    const patchStub =(patch: Partial<Message>) => {
      queryClient.setQueryData<MessagesQueryData>(queryKey, (old) => {
        if (!old) {
          return { messages: [], nextCursor: null };
        }
        return {
          ...old,
          messages: old.messages.map((m) =>
            m.id === optimisticId ? { ...m, ...patch } : m
          ),
        };
      });
    };

    // Named first (#1335) so a refetch of the stored row drops the stub; the outbox uploads attachments.
    let messageId: string;
    let outgoing: OutgoingAttachment[];
    try {
      messageId = await invoke<string>("new_message_id");
      patchStub({ outbox_id: messageId });
      outgoing = await outgoingAttachments(attachments);
    } catch (error) {
      turn.release();
      await releaseStaged(attachments);
      patchStub({ status: 'failed', retryable: false });
      console.error("Failed to send message:", error);
      return;
    }

    try {
      await sendMessageMutation.mutateAsync({
        channelId: selectedChannelId || "",
        conversationId: selectedConversationId || "",
        content: contentText,
        attachments: outgoing,
        replyToMessageId: replyToMessageId ?? undefined,
        optimisticId,
        messageId,
        turn,
      });
    } catch (error) {
      await releaseStaged(attachments);
      // The core marked it failed; the refetched row offers Retry.
      patchStub({ status: 'failed' });
      console.error("Failed to send message:", error);
    }
  };

  if (!selectedChannelId && !selectedConversationId) {
    return (
      <EmptyState testId="main-content" messageTestId="empty-channel-message">
        {t("chat.noSelection")}
      </EmptyState>
    );
  }

  return (
    <div
      data-testid="main-content"
      className="flex-1 flex flex-col overflow-hidden min-w-0 bg-bg"
    >
      <div className="flex-1 flex flex-col overflow-hidden min-h-0">
        {messagesLoading ? (
          <div className="flex-1 flex items-center justify-center">
            <LoadingSpinner size="base" />
          </div>
        ) : (
          <MessageList
            messages={channelMessages}
            // MLS group id (for DMs, the dm_channel_id).
            conversationId={selectedGroupId ?? selectedConversationId ?? null}
            groupIdForNames={selectedGroupId ?? null}
            adminUserIds={selectedGroupId ? adminUserIds : undefined}
            viewerIsAdmin={viewerIsAdmin}
            onOpenThread={openThread}
            onVerifyKeyChange={setSafety}
            threadReplyCounts={threadReplyCounts}
            onReply={handleReply}
            onEdit={handleEdit}
            onDelete={handleDelete}
            pinsConversationId={selectedChannelId ?? selectedConversationId ?? null}
            onScrollToMessage={handleScrollToMessage}
            getAuthorUsername={getAuthorUsername}
            hasMore={!!pageCursor}
            isFetchingMore={loadingMore}
            onLoadMore={loadMore}
            hasNewer={!!newerCursor}
            isFetchingNewer={loadingNewer}
            onLoadNewer={loadNewer}
            windowEpoch={windowEpoch}
            focusComposer={focusComposer}
          />
        )}
      </div>

      {replyToMessageId && (
        <ReplyPreview
          messageId={replyToMessageId}
          allMessages={allMessages}
          onDismiss={() => setReplyToMessageId(null)}
          onScrollToMessage={handleScrollToMessage}
        />
      )}

      <MessageQueue />

      {pendingDmRequest ? (
        <div data-testid="dm-request-bar">
          <div
            className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface"
          >
            <span className="flex-1 text-2xs font-mono uppercase tracking-widest text-muted">
              {t("dmRequest.heading")}
            </span>
          </div>
          <div
            className="flex items-center justify-between gap-4 px-4 pb-3 pt-2 bg-surface"
          >
            <p className="text-xs font-mono text-dim">
              <Trans
                ns="nav"
                i18nKey="dmRequest.body"
                values={{ name: pendingDmRequest.senderName }}
                components={{ name: <span className="text-fg" /> }}
              />
            </p>
            <div className="flex items-center gap-2 flex-shrink-0">
              <Button
                data-testid="dm-request-accept"
                variant="primary"
                onClick={handleAcceptDmRequest}
                isLoading={acceptDmRequestMutation.isPending}
                loadingText={t("dmRequest.accepting")}
                disabled={acceptDmRequestMutation.isPending || blockUserMutation.isPending}
                autoFocus
              >
                {t("dmRequest.accept")}
              </Button>
              <Button
                data-testid="dm-request-block"
                variant="secondary"
                onClick={handleBlockDmRequest}
                isLoading={blockUserMutation.isPending}
                loadingText={t("dmRequest.blocking")}
                disabled={acceptDmRequestMutation.isPending || blockUserMutation.isPending}
              >
                {t("dmRequest.block")}
              </Button>
            </div>
          </div>
        </div>
      ) : editingMessage ? (
        <EditMessageBar
          key={editingMessage.id}
          testId="edit-message-bar"
          inputTestId="edit-message-bar-input"
          cancelTestId="cancel-edit-button"
          heading={t("editBar.heading")}
          cancelLabel={t("editBar.cancel")}
          hint={t("editBar.hint")}
          value={editDraftValue}
          onChange={setEditDraftValue}
          onSave={handleSaveEdit}
          onCancel={handleCancelEdit}
          isSaving={editMessageMutation.isPending}
        />
      ) : isDeletingThisChannel ? (
        <div data-testid="delete-channel-bar">
          <div
            className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface"
          >
            <span className="flex-1 text-2xs font-mono uppercase tracking-widest text-muted">
              {t("deleteChannel.heading")}
            </span>
            <button
              data-testid="delete-channel-cancel"
              onClick={() => setPendingDeleteChannelId(null)}
              aria-label={t("deleteChannel.cancel")}
              className="icon-btn-sm flex-shrink-0"
            >
              <X size={20} aria-hidden="true" />
            </button>
          </div>
          <div
            className="flex items-center justify-between gap-4 px-4 pb-3 pt-2 bg-surface"
          >
            <p className="text-xs font-mono text-dim">
              {t("deleteChannel.body")}
            </p>
            <Button
              data-testid="delete-channel-confirm"
              variant="danger"
              onClick={handleConfirmDeleteChannel}
              isLoading={deleteChannelMutation.isPending}
              loadingText={t("deleteChannel.deleting")}
              autoFocus
            >
              {t("common:actions.delete")}
            </Button>
          </div>
        </div>
      ) : pendingDeleteId ? (() => {
        const target = allMessages.find((m) => m.id === pendingDeleteId);
        const isModerating = !!target && !!currentUser && target.sender_id !== currentUser.id;
        const heading = isModerating
          ? t("deleteMessage.moderateHeading")
          : t("deleteMessage.heading");
        const body = isModerating
          ? t("deleteMessage.moderateBody")
          : t("deleteMessage.body");
        return (
          <div data-testid="delete-message-bar">
            <div
              className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface"
            >
              <span className="flex-1 text-2xs font-mono uppercase tracking-widest text-muted">
                {heading}
              </span>
              <button
                data-testid="delete-message-cancel"
                onClick={() => setPendingDeleteId(null)}
                aria-label={t("deleteMessage.cancel")}
                className="icon-btn-sm flex-shrink-0"
              >
                <X size={20} aria-hidden="true" />
              </button>
            </div>
            <div
              className="flex items-center justify-between gap-4 px-4 pb-3 pt-2 bg-surface"
            >
              <p className="text-xs font-mono text-dim">
                {body}
              </p>
              <Button
                data-testid="delete-message-confirm"
                variant="danger"
                onClick={handleConfirmDelete}
                isLoading={deleteMessageMutation.isPending}
                loadingText={t("deleteMessage.deleting")}
                autoFocus
              >
                {isModerating ? t("deleteMessage.remove") : t("common:actions.delete")}
              </Button>
            </div>
          </div>
        );
      })() : (
        <div data-testid="message-form">
          <TypingIndicator
            channelId={selectedChannelId ?? null}
            conversationId={selectedChannelId ? null : selectedConversationId ?? null}
          />
          <ChatInput
            ref={chatInputRef}
            onSend={handleSend}
            onValueChange={typing.notify}
            autoFocus
            onHistoryUp={() => {
              messageNavStore.dispatch({ type: "enter" });
              return messageNavStore.active;
            }}
            // @all fans out a notification only in group channels (DMs don't),
            // so the live "@all notifies everyone" hint is gated on one.
            canNotifyAll={!!selectedChannelId}
            draftKey={
              // Room-kind prefix keeps channel and DM draft slots apart.
              selectedChannelId
                ? `channel:${selectedChannelId}`
                : selectedConversationId
                  ? `conv:${selectedConversationId}`
                  : null
            }
          />
        </div>
      )}
    </div>
  );
});
