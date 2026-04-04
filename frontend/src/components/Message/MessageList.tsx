import React, { useEffect, useRef, useMemo } from "react";
import { MessageItem } from "./MessageItem";
import type { Message } from "../../types";

interface MessageListProps {
  messages: Message[];
  onReply?: (messageId: string) => void;
  onPin?: (messageId: string) => void;
  onScrollToMessage?: (messageId: string) => void;
  getAuthorUsername?: (authorId: string, message?: Message) => string;
}

export const MessageList: React.FC<MessageListProps> = ({
  messages,
  onReply,
  onScrollToMessage,
  getAuthorUsername,
}) => {
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const prevLengthRef = useRef(0);

  const sortedMessages = useMemo(
    () =>
      [...messages]
        // Filter out blank messages — but keep any message with attachments even
        // if the text caption is empty.
        .filter((m) => {
          const hasText = m.content_decrypted != null && m.content_decrypted !== "";
          const hasAttachments = (m.attachments?.length ?? 0) > 0;
          // content_decrypted === undefined means decryption failed — keep the
          // message so it renders as [encrypted] instead of vanishing silently.
          const decryptionFailed = m.content_decrypted === undefined;
          return hasText || hasAttachments || decryptionFailed;
        })
        .sort((a, b) => a.created_at - b.created_at),
    [messages]
  );

  // Scroll to bottom when new messages arrive
  useEffect(() => {
    if (sortedMessages.length > prevLengthRef.current) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    prevLengthRef.current = sortedMessages.length;
  }, [sortedMessages.length]);

  const scrollToMessage = (messageId: string) => {
    const el = containerRef.current?.querySelector(`[data-testid="message-${messageId}"]`);
    if (el) {
      el.scrollIntoView({ behavior: "smooth", block: "center" });
    }
    onScrollToMessage?.(messageId);
  };

  if (sortedMessages.length === 0) {
    return (
      <div
        data-testid="empty-messages"
        className="flex-1 flex items-center justify-center"
        style={{ background: "var(--c-bg)" }}
      >
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          No messages yet. Start the conversation.
        </p>
      </div>
    );
  }

  return (
    <div
      data-testid="message-list"
      ref={containerRef}
      className="flex-1 overflow-y-auto min-h-0"
      style={{ background: "var(--c-bg)" }}
    >
      {sortedMessages.map((message) => {
        const authorUsername = getAuthorUsername
          ? getAuthorUsername(message.sender_id, message)
          : "Unknown";
        return (
          <MessageItem
            key={message.id}
            message={message}
            allMessages={sortedMessages}
            authorUsername={authorUsername}
            onReply={onReply}
            onScrollToReply={scrollToMessage}
          />
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
};
