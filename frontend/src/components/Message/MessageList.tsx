import React, { useEffect, useLayoutEffect, useRef, useMemo } from "react";
import { MessageItem } from "./MessageItem";
import type { Message } from "../../types";

interface MessageListProps {
  messages: Message[];
  adminUserIds?: Set<string>;
  onReply?: (messageId: string) => void;
  onEdit?: (messageId: string, newContent: string) => Promise<void>;
  onDelete?: (messageId: string) => void;
  onPin?: (messageId: string) => void;
  onScrollToMessage?: (messageId: string) => void;
  getAuthorUsername?: (authorId: string, message?: Message) => string;
  hasMore?: boolean;
  isFetchingMore?: boolean;
  onLoadMore?: () => void;
}

export const MessageList: React.FC<MessageListProps> = ({
  messages,
  adminUserIds,
  onReply,
  onEdit,
  onDelete,
  onScrollToMessage,
  getAuthorUsername,
  hasMore,
  isFetchingMore,
  onLoadMore,
}) => {
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const prevLengthRef = useRef(0);
  // Saved scroll metrics taken just before a load-more fetch begins, used to
  // restore relative scroll position after older messages are prepended.
  const savedScrollRef = useRef<{ scrollTop: number; scrollHeight: number } | null>(null);

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
          // Soft-deleted messages have no content but should remain visible as "[deleted]".
          const isSoftDeleted = !!m.deleted_at;
          return hasText || hasAttachments || decryptionFailed || isSoftDeleted;
        })
        .sort((a, b) => a.created_at - b.created_at),
    [messages]
  );

  // Scroll to bottom when new messages arrive (but not when older pages load).
  useEffect(() => {
    if (sortedMessages.length > prevLengthRef.current && !isFetchingMore) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    prevLengthRef.current = sortedMessages.length;
  }, [sortedMessages.length, isFetchingMore]);

  // When a load-more fetch starts, save current scroll metrics so position can
  // be restored after older messages are prepended.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    if (isFetchingMore) {
      savedScrollRef.current = {
        scrollTop: container.scrollTop,
        scrollHeight: container.scrollHeight,
      };
    }
  }, [isFetchingMore]);

  // After the message list grows following a load-more, restore relative scroll
  // so the previously visible messages stay in view.
  useLayoutEffect(() => {
    const container = containerRef.current;
    const saved = savedScrollRef.current;
    if (!container || !saved || isFetchingMore) {
      return;
    }
    const heightDelta = container.scrollHeight - saved.scrollHeight;
    if (heightDelta > 0) {
      container.scrollTop = saved.scrollTop + heightDelta;
      savedScrollRef.current = null;
    }
  }, [sortedMessages.length, isFetchingMore]);

  // Trigger load-more when the user scrolls near the top.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    const handleScroll = () => {
      if (container.scrollTop < 150 && hasMore && !isFetchingMore) {
        onLoadMore?.();
      }
    };
    container.addEventListener("scroll", handleScroll, { passive: true });
    return () => container.removeEventListener("scroll", handleScroll);
  }, [hasMore, isFetchingMore, onLoadMore]);

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
      {isFetchingMore && (
        <p
          className="text-xs font-mono text-center py-2"
          style={{ color: "var(--c-text-muted)" }}
        >
          Loading…
        </p>
      )}
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
            isAuthorAdmin={adminUserIds?.has(message.sender_id) ?? false}
            onReply={onReply}
            onEdit={onEdit}
            onDelete={onDelete}
            onScrollToReply={scrollToMessage}
          />
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
};
