import React from "react";
import { useAppStore } from "../../stores/appStore";
import { MessageList } from "../Message/MessageList";
import { ReplyPreview } from "../Message/ReplyPreview";
import { MessageQueue } from "../Message/MessageQueue";
import { ChatInput } from "../ui/ChatInput";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { useMessages, useSendMessage } from "../../hooks/queries";

export const MainContent: React.FC = () => {
  const {
    selectedChannelId,
    selectedConversationId,
    replyToMessageId,
    setReplyToMessageId,
    currentUser,
  } = useAppStore();

  const { data: messages = [], isLoading: messagesLoading } = useMessages(
    selectedChannelId,
    selectedConversationId
  );
  const sendMessageMutation = useSendMessage();

  const handleSend = async (text: string) => {
    if (!text.trim() || (!selectedChannelId && !selectedConversationId) || !currentUser) {
      return;
    }
    try {
      await sendMessageMutation.mutateAsync({
        channelId: selectedChannelId || "",
        conversationId: selectedConversationId || "",
        content: text.trim(),
        replyToMessageId: replyToMessageId || undefined,
      });
      setReplyToMessageId(null);
    } catch (error) {
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

  console.log({currentUser})
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
              authorId === currentUser?.id
                ? (currentUser as any).username || "you"
                : message?.sender_username || authorId
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
        <ChatInput onSend={(text) => handleSend(text)} autoFocus />
      </div>
    </div>
  );
};
