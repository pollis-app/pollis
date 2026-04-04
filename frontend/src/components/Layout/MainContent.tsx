import React from "react";
import { useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import { MessageList } from "../Message/MessageList";
import { ReplyPreview } from "../Message/ReplyPreview";
import { MessageQueue } from "../Message/MessageQueue";
import { ChatInput, type Attachment } from "../ui/ChatInput";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { useMessages, useSendMessage, messageQueryKeys } from "../../hooks/queries";
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

export const MainContent: React.FC = () => {
  const {
    selectedChannelId,
    selectedConversationId,
    replyToMessageId,
    setReplyToMessageId,
    currentUser,
  } = useAppStore();

  const queryClient = useQueryClient();
  const { data: messages = [], isLoading: messagesLoading } = useMessages(
    selectedChannelId,
    selectedConversationId
  );
  const sendMessageMutation = useSendMessage();

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
    queryClient.setQueryData<Message[]>(queryKey, (old) =>
      old ? [...old, optimisticMessage] : [optimisticMessage]
    );
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
        replyToMessageId: undefined,
        optimisticId,
      });
    } catch (error) {
      // Mark the optimistic stub as failed so the user can see it.
      queryClient.setQueryData<Message[]>(queryKey, (old) =>
        old
          ? old.map((m) => (m.id === optimisticId ? { ...m, status: 'failed' as const } : m))
          : []
      );
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
            messages={messages}
            onReply={(id) => setReplyToMessageId(id)}
            onScrollToMessage={(id) => console.log("Scroll to:", id)}
            getAuthorUsername={(authorId, message) =>
              message?.sender_username || (authorId === currentUser?.id ? (currentUser?.username ?? authorId) : authorId)
            }
          />
        )}
      </div>

      {replyToMessageId && (
        <ReplyPreview
          messageId={replyToMessageId}
          allMessages={messages}
          onDismiss={() => setReplyToMessageId(null)}
          onScrollToMessage={(id) => console.log("Scroll to:", id)}
        />
      )}

      <MessageQueue />

      <div data-testid="message-form">
        <ChatInput onSend={handleSend} autoFocus />
      </div>
    </div>
  );
};
