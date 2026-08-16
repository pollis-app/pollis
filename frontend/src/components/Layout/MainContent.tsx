import React, { useCallback, useRef, useMemo, useState, useEffect } from "react";
import { Trans, useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
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
import { useMessages, useSendMessage, messageQueryKeys, useDeleteMessage, useEditMessage, useAcceptDMRequest, useBlockUser } from "../../hooks/queries";
import { transformChannelMessage, type RawChannelMessage, useThreadSummaries } from "../../hooks/queries/useMessages";
import { useRightPanel } from "./RightPanel/useRightPanel";
import { useGroupMembers, useDeleteChannel } from "../../hooks/queries/useGroups";
import type { Message, MessageAttachment } from "../../types";
import { buildMessageContent } from "../../utils/attachmentEnvelope";
import { useTypingPublisher } from "../../hooks/useTypingPublisher";
import { messageNavStore } from "../../stores/messageNavStore";
import { TypingIndicator } from "../TypingIndicator";
import { EmptyState } from "../ui/EmptyState";

// Passed from DM page when the current user has not yet accepted the DM.
// Replaces the chat input with an accept/block bar.
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

interface MainContentProps {
  pendingDmRequest?: PendingDmRequest | null;
}

export const MainContent: React.FC<MainContentProps> = observer(({ pendingDmRequest = null }) => {
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
  const deleteChannelMutation = useDeleteChannel();
  const isDeletingThisChannel =
    !!selectedChannelId && pendingDeleteChannelId === selectedChannelId;

  const { data: groupMembers = [] } = useGroupMembers(selectedGroupId ?? null);
  const adminUserIds = useMemo(
    () => new Set(groupMembers.filter((m) => m.role === "admin").map((m) => m.user_id)),
    [groupMembers],
  );
  // Viewer is an admin in this channel's group — gates the moderator
  // delete affordance on other members' messages.
  const viewerIsAdmin =
    !!selectedGroupId && !!currentUser && adminUserIds.has(currentUser.id);

  const chatInputRef = useRef<ChatInputHandle>(null);

  // For channels the LiveKit room is the parent group's MLS group id; for
  // DMs it's the conversation id directly. `useTypingPublisher` no-ops when
  // the room id is null (e.g. nothing selected) so we don't need to gate
  // the hook call.
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
  // Threads (#825). `openThread` writes the panel search params; the summaries
  // give each root message its "N replies" count without loading the thread.
  const { openThread } = useRightPanel();
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

  // ID of message pending delete confirmation (null = no dialog).
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  // Message currently being edited (null = not editing).
  const [editingMessage, setEditingMessage] = useState<Message | null>(null);
  const [editDraftValue, setEditDraftValue] = useState('');
  const [editBarFocused, setEditBarFocused] = useState(false);
  const editTextareaRef = useRef<HTMLTextAreaElement>(null);

  const [olderMessages, setOlderMessages] = useState<Message[]>([]);
  const [loadingMore, setLoadingMore] = useState(false);
  const [pageCursor, setPageCursor] = useState<PageCursor | null>(null);

  // Reset pagination and edit state when the selected channel/conversation changes.
  useEffect(() => {
    setOlderMessages([]);
    setPageCursor(null);
    setEditingMessage(null);
  }, [selectedChannelId, selectedConversationId]);

  // Focus the edit textarea and place cursor at end when entering edit mode.
  useEffect(() => {
    if (!editingMessage) {
      return;
    }
    const el = editTextareaRef.current;
    if (!el) {
      return;
    }
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, [editingMessage]);

  // Escape cancels edit/delete/reply bar — capture phase so AppShell's navigation handler doesn't fire first.
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

  // Initialise the cursor from the initial page load (only if no older pages
  // have been fetched yet — don't overwrite cursor mid-pagination).
  // Include the selected channel/conversation so the cursor re-initialises on
  // switch — keyed on nextCursor alone it could miss a re-init when the new
  // channel's initial cursor equals the previous one.
  useEffect(() => {
    if (nextCursor && olderMessages.length === 0) {
      setPageCursor(nextCursor);
    }
  }, [nextCursor, selectedChannelId, selectedConversationId]);

  // MessageItem dispatches this when an attachment lightbox closes so focus
  // returns to the chat input — keeps the keyboard-driven flow intact.
  useEffect(() => {
    const handler = () => chatInputRef.current?.focus();
    window.addEventListener("pollis:focus-chat-input", handler);
    return () => window.removeEventListener("pollis:focus-chat-input", handler);
  }, []);

  // Merge older fetched pages with the live initial page, deduplicated and
  // sorted oldest-first. Dedup keeps the first occurrence by message ID.
  const allMessages = useMemo(() => {
    const combined = [...olderMessages, ...messages];
    const seen = new Set<string>();
    const deduped: Message[] = [];
    for (const m of combined) {
      if (!seen.has(m.id)) {
        seen.add(m.id);
        deduped.push(m);
      }
    }
    return deduped.sort((a, b) => a.created_at - b.created_at);
  }, [olderMessages, messages]);

  // What the CHANNEL shows: everything except thread replies (#825). A reply
  // lives in its thread, and the root already advertises it with "N replies" —
  // showing it in both places double-posts every reply into the channel, which
  // is exactly what threads exist to avoid.
  //
  // Filtered here rather than in `allMessages` on purpose: that list still
  // backs the two id lookups that must resolve a reply which is only rendered
  // in the thread panel — the edit/delete confirmation (`allMessagesRef`) and
  // the composer's `ReplyPreview` strip.
  //
  // The quote rendered INSIDE a row is not one of them: `MessageList` builds
  // its own `replyTargets` index from the list it was handed, i.e. this
  // filtered one. (The comment here used to claim otherwise — #874.)
  const channelMessages = useMemo(
    () => allMessages.filter((m) => !m.thread_id || m.thread_id === m.id),
    [allMessages],
  );

  const loadMore = useCallback(async () => {
    if (!pageCursor || loadingMore || !currentUser) {
      return;
    }
    setLoadingMore(true);
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

      setOlderMessages((prev) => {
        const existingIds = new Set(prev.map((m) => m.id));
        const newOnes = fetched.filter((m) => !existingIds.has(m.id));
        return [...newOnes, ...prev];
      });
      setPageCursor(page.next_cursor ?? null);
    } finally {
      setLoadingMore(false);
    }
  }, [pageCursor, loadingMore, currentUser, selectedChannelId, selectedConversationId]);

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

  // The four callbacks below reach `MessageList` and, through it, every row.
  // They are `useCallback`-pinned because this component re-renders on every
  // keystroke in the edit bar (`editDraftValue` is local state) — with fresh
  // identities that keystroke re-rendered the entire message log (#874).
  // `allMessagesRef` keeps `handleEdit` stable across message arrivals too.
  const allMessagesRef = useRef(allMessages);
  allMessagesRef.current = allMessages;

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

  // Depends on `currentUser` only — the per-message branch reads its argument.
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

    const contentText = text.trim();
    const queryKey = selectedChannelId
      ? messageQueryKeys.channel(selectedChannelId)
      : messageQueryKeys.conversation(selectedConversationId!);

    // Build optimistic attachment stubs so the message renders immediately.
    // Images use the local preview blob URL; videos/files show a pending indicator.
    const optimisticAttachments: MessageAttachment[] = attachments.map((att) => ({
      id: att.id,
      object_key: '',
      content_hash: '',
      filename: att.name,
      content_type: att.mimeType,
      file_size: att.size,
      uploaded_at: Date.now(),
      localPreviewUrl: att.preview,
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

    // Render immediately — upload + encrypt happens in the background.
    queryClient.setQueryData<MessagesQueryData>(queryKey, (old) => {
      const prev = old ?? { messages: [], nextCursor: null };
      return { ...prev, messages: [...prev.messages, optimisticMessage] };
    });
    setReplyToMessageId(null);

    try {
      const content = await buildMessageContent(attachments, contentText);

      await sendMessageMutation.mutateAsync({
        channelId: selectedChannelId || "",
        conversationId: selectedConversationId || "",
        content,
        replyToMessageId: replyToMessageId ?? undefined,
        optimisticId,
      });
    } catch (error) {
      // Mark the optimistic stub as failed so the user can see it.
      queryClient.setQueryData<MessagesQueryData>(queryKey, (old) => {
        if (!old) {
          return { messages: [], nextCursor: null };
        }
        return {
          ...old,
          messages: old.messages.map((m) =>
            m.id === optimisticId ? { ...m, status: 'failed' as const } : m
          ),
        };
      });
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
            // MLS group id for the open conversation. Groups: selectedGroupId.
            // DMs: selectedConversationId (the dm_channel_id IS the MLS group
            // id for that DM). Either way, this is what RosterChanged events
            // are keyed by — the rosterChangeStore lookup matches.
            conversationId={selectedGroupId ?? selectedConversationId ?? null}
            // Display-name resolution for roster banners. Only group MLS
            // has a member list; DMs are 1:1 so banner names there fall
            // back to user_id (no membership churn anyway).
            groupIdForNames={selectedGroupId ?? null}
            adminUserIds={selectedGroupId ? adminUserIds : undefined}
            viewerIsAdmin={viewerIsAdmin}
            onOpenThread={openThread}
            threadReplyCounts={threadReplyCounts}
            onReply={handleReply}
            onEdit={handleEdit}
            onDelete={handleDelete}
            // TODO: scroll-to-message not yet implemented; prop left unwired
            getAuthorUsername={getAuthorUsername}
            hasMore={!!pageCursor}
            isFetchingMore={loadingMore}
            onLoadMore={loadMore}
            focusComposer={focusComposer}
          />
        )}
      </div>

      {replyToMessageId && (
        <ReplyPreview
          messageId={replyToMessageId}
          allMessages={allMessages}
          onDismiss={() => setReplyToMessageId(null)}
          // TODO: scroll-to-message not yet implemented; prop left unwired
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
        <div data-testid="edit-message-bar">
          <div
            className="flex items-center gap-2 px-4 py-1.5 flex-shrink-0 border-t border-line bg-surface"
          >
            <span className="flex-1 text-2xs font-mono uppercase tracking-widest text-muted">
              {t("editBar.heading")}
            </span>
            <button
              data-testid="cancel-edit-button"
              onClick={handleCancelEdit}
              aria-label={t("editBar.cancel")}
              className="icon-btn-sm flex-shrink-0"
            >
              <X size={20} aria-hidden="true" />
            </button>
          </div>
          <div className="px-4 pb-3 pt-1 bg-surface">
            <textarea
              ref={editTextareaRef}
              data-testid="edit-message-bar-input"
              value={editDraftValue}
              onChange={(e) => setEditDraftValue(e.target.value)}
              onFocus={() => setEditBarFocused(true)}
              onBlur={() => setEditBarFocused(false)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  handleSaveEdit();
                }
              }}
              disabled={editMessageMutation.isPending}
              rows={2}
              className={`chat-input-textarea w-full font-mono text-sm resize-none transition-colors${editBarFocused ? " is-focused" : ""}`}
              style={{
                borderRadius: '4px',
                border: 'none',
                outline: 'none',
                padding: '4px 8px',
                background: editBarFocused ? 'var(--c-accent)' : 'var(--c-hover)',
                color: editBarFocused ? 'var(--c-bg)' : 'var(--c-text)',
                opacity: editMessageMutation.isPending ? 0.5 : 1,
              }}
            />
            <p className="text-2xs font-mono mt-1 text-muted">
              {t("editBar.hint")}
            </p>
          </div>
        </div>
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
            // ArrowUp from an empty/first-line composer walks up the message
            // log (bash-history style). Claimed only when the log actually
            // has a message to focus.
            onHistoryUp={() => {
              messageNavStore.dispatch({ type: "enter" });
              return messageNavStore.active;
            }}
            // @all fans out a notification only in group channels (DMs don't),
            // so the live "@all notifies everyone" hint is gated on one.
            canNotifyAll={!!selectedChannelId}
            draftKey={
              // Prefix with the room kind so the rare case of a channel id
              // and a DM conversation id colliding still routes to separate
              // draft slots. Falls back to null when nothing is selected —
              // ChatInput then skips draft persistence entirely.
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
