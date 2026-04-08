import React, { useRef, useMemo, useState, useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import { MessageList } from "../Message/MessageList";
import { ReplyPreview } from "../Message/ReplyPreview";
import { MessageQueue } from "../Message/MessageQueue";
import { ChatInput, type Attachment, type ChatInputHandle } from "../ui/ChatInput";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { Button } from "../ui/Button";
import { useMessages, useSendMessage, messageQueryKeys, useDeleteMessage, useEditMessage } from "../../hooks/queries";
import { transformChannelMessage } from "../../hooks/queries/useMessages";
import { useGroupMembers } from "../../hooks/queries/useGroups";
import type { Message, MessageAttachment } from "../../types";
import { blurhashFromUrl } from "../../utils/imageProcessing";

type MediaUploadResult = {
  key: string;
  url: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  content_hash: string;
  blurhash?: string;
  width?: number;
  height?: number;
};

type PageCursor = { sent_at: string; id: string };

type RawMessagePage = {
  messages: unknown[];
  next_cursor: PageCursor | null;
};

type MessagesQueryData = {
  messages: Message[];
  nextCursor: PageCursor | null;
};

export const MainContent: React.FC = () => {
  const {
    selectedChannelId,
    selectedConversationId,
    selectedGroupId,
    replyToMessageId,
    setReplyToMessageId,
    currentUser,
  } = useAppStore();

  const { data: groupMembers = [] } = useGroupMembers(selectedGroupId ?? null);
  const adminUserIds = useMemo(
    () => new Set(groupMembers.filter((m) => m.role === "admin").map((m) => m.user_id)),
    [groupMembers],
  );

  const chatInputRef = useRef<ChatInputHandle>(null);

  const queryClient = useQueryClient();
  const { messages, nextCursor, isLoading: messagesLoading } = useMessages(
    selectedChannelId,
    selectedConversationId
  );
  const sendMessageMutation = useSendMessage();
  const deleteMessageMutation = useDeleteMessage();
  const editMessageMutation = useEditMessage();

  // ID of message pending delete confirmation (null = no dialog).
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);

  const [olderMessages, setOlderMessages] = useState<Message[]>([]);
  const [loadingMore, setLoadingMore] = useState(false);
  const [pageCursor, setPageCursor] = useState<PageCursor | null>(null);

  // Reset pagination state when the selected channel/conversation changes.
  useEffect(() => {
    setOlderMessages([]);
    setPageCursor(null);
  }, [selectedChannelId, selectedConversationId]);

  // Initialise the cursor from the initial page load (only if no older pages
  // have been fetched yet — don't overwrite cursor mid-pagination).
  useEffect(() => {
    if (nextCursor && olderMessages.length === 0) {
      setPageCursor(nextCursor);
    }
  }, [nextCursor]);

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

  const loadMore = async () => {
    if (!pageCursor || loadingMore || !currentUser) {
      return;
    }
    setLoadingMore(true);
    try {
      let page: RawMessagePage;
      if (selectedChannelId) {
        page = await invoke<RawMessagePage>('get_channel_messages', {
          userId: currentUser.id,
          channelId: selectedChannelId,
          limit: 50,
          cursor: pageCursor,
        });
      } else if (selectedConversationId) {
        page = await invoke<RawMessagePage>('get_dm_messages', {
          userId: currentUser.id,
          dmChannelId: selectedConversationId,
          limit: 50,
          cursor: pageCursor,
        });
      } else {
        return;
      }

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const fetched = (page.messages as any[]).map(transformChannelMessage);

      setOlderMessages((prev) => {
        const existingIds = new Set(prev.map((m) => m.id));
        const newOnes = fetched.filter((m) => !existingIds.has(m.id));
        return [...newOnes, ...prev];
      });
      setPageCursor(page.next_cursor ?? null);
    } finally {
      setLoadingMore(false);
    }
  };

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

  const handleEdit = async (messageId: string, newContent: string) => {
    const conversationId = selectedChannelId ?? selectedConversationId;
    if (!conversationId) {
      return;
    }
    await editMessageMutation.mutateAsync({ conversationId, messageId, newContent });
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
      let content = contentText;

      if (attachments.length > 0) {
        // For video attachments that have a poster preview, compute a blurhash
        // so receivers see a placeholder without downloading the video first.
        const videoBlurhashes = new Map<string, { bh: string; w: number; h: number }>();
        await Promise.all(
          attachments
            .filter((att) => att.mimeType.startsWith("video/") && att.preview)
            .map(async (att) => {
              const meta = await blurhashFromUrl(att.preview!).catch(() => null);
              if (meta) {
                videoBlurhashes.set(att.id, { bh: meta.hash, w: meta.width, h: meta.height });
              }
            })
        );

        const results = await Promise.all(
          attachments.map((att) =>
            invoke<MediaUploadResult>('upload_media', {
              path: att.path,
              filename: att.name,
              contentType: att.mimeType,
            })
          )
        );

        const envelope: Record<string, unknown> = {
          _att: results.map((r, i) => {
            const vMeta = videoBlurhashes.get(attachments[i]?.id ?? "");
            return {
              key: r.key,
              url: r.url,
              name: r.filename,
              ct: r.content_type,
              size: r.size_bytes,
              hash: r.content_hash,
              bh: r.blurhash ?? vMeta?.bh,
              w: r.width ?? vMeta?.w,
              h: r.height ?? vMeta?.h,
            };
          }),
        };
        if (contentText) {
          envelope._txt = contentText;
        }
        content = JSON.stringify(envelope);
      }

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
      <div
        data-testid="main-content"
        className="flex-1 flex items-center justify-center"
        style={{ background: 'var(--c-bg)' }}
      >
        <p
          data-testid="empty-channel-message"
          className="text-xs font-mono"
          style={{ color: 'var(--c-text-muted)' }}
        >
          Select a channel to start messaging
        </p>
      </div>
    );
  }

  return (
    <div
      data-testid="main-content"
      className="flex-1 flex flex-col overflow-hidden min-w-0"
      style={{ background: 'var(--c-bg)' }}
    >
      <div className="flex-1 flex flex-col overflow-hidden min-h-0">
        {messagesLoading ? (
          <div className="flex-1 flex items-center justify-center">
            <LoadingSpinner size="base" />
          </div>
        ) : (
          <MessageList
            messages={allMessages}
            adminUserIds={selectedGroupId ? adminUserIds : undefined}
            onReply={(id) => {
              setReplyToMessageId(id);
              chatInputRef.current?.focus();
            }}
            onEdit={handleEdit}
            onDelete={(id) => setPendingDeleteId(id)}
            onScrollToMessage={(id) => console.log("Scroll to:", id)}
            getAuthorUsername={(authorId, message) =>
              message?.sender_username || (authorId === currentUser?.id ? (currentUser?.username ?? authorId) : authorId)
            }
            hasMore={!!pageCursor}
            isFetchingMore={loadingMore}
            onLoadMore={loadMore}
          />
        )}
      </div>

      {replyToMessageId && (
        <ReplyPreview
          messageId={replyToMessageId}
          allMessages={allMessages}
          onDismiss={() => setReplyToMessageId(null)}
          onScrollToMessage={(id) => console.log("Scroll to:", id)}
        />
      )}

      <MessageQueue />

      <div data-testid="message-form">
        <ChatInput ref={chatInputRef} onSend={handleSend} autoFocus />
      </div>

      {/* Delete confirmation overlay */}
      {pendingDeleteId && (
        <div
          data-testid="delete-message-overlay"
          style={{
            position: "fixed",
            inset: 0,
            zIndex: 9999,
            background: "rgba(0,0,0,0.7)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}
          onClick={() => setPendingDeleteId(null)}
        >
          <div
            className="flex flex-col gap-4 p-6"
            style={{
              background: "var(--c-surface)",
              border: "1px solid var(--c-border)",
              borderRadius: "8px",
              minWidth: 280,
              maxWidth: 380,
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
              Delete this message? It will be removed from your view.
              Others who already received it may still see it.
            </p>
            {deleteMessageMutation.isError && (
              <p className="text-xs font-mono" style={{ color: "#ff6b6b" }}>
                Failed to delete. Please try again.
              </p>
            )}
            <div className="flex gap-3 justify-end">
              <Button
                data-testid="delete-message-cancel"
                variant="secondary"
                onClick={() => setPendingDeleteId(null)}
                disabled={deleteMessageMutation.isPending}
              >
                Cancel
              </Button>
              <Button
                data-testid="delete-message-confirm"
                variant="danger"
                onClick={handleConfirmDelete}
                isLoading={deleteMessageMutation.isPending}
                loadingText="Deleting…"
              >
                Delete
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};
