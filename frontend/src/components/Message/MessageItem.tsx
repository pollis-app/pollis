import React, { useState, useEffect } from "react";
import { Reply, Download, Image as ImageIcon, File as FileIcon } from "lucide-react";
import { useAppStore } from "../../stores/appStore";
import { getFileDownloadUrl } from "../../services/r2-upload";
// import { MessageReactions } from "./MessageReactions";
import type { Message, MessageAttachment } from "../../types";

interface MessageItemProps {
  message: Message;
  allMessages?: Message[];
  authorUsername?: string;
  onReply?: (messageId: string) => void;
  onPin?: (messageId: string) => void;
  onScrollToReply?: (messageId: string) => void;
}

const formatTimestamp = (timestamp: number): string => {
  const tsMs = timestamp < 1e12 ? timestamp * 1000 : timestamp;
  const d = new Date(tsMs);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
};

export const MessageItem: React.FC<MessageItemProps> = ({
  message,
  allMessages = [],
  authorUsername = "unknown",
  onReply,
  onScrollToReply,
}) => {
  const { currentUser } = useAppStore();
  const isOwn = message.sender_id === currentUser?.id;

  const replyTo = message.reply_to_message_id
    ? allMessages.find((m) => m.id === message.reply_to_message_id)
    : null;

  const replyToAuthor = replyTo
    ? (replyTo.sender_username ?? replyTo.sender_id)
    : null;

  const content =
    message.content_decrypted ??
    (message as any).content ??
    (message.ciphertext ? "[encrypted]" : "");

  return (
    <div
      data-testid={`message-${message.id}`}
      aria-label={`Message from ${authorUsername}`}
      className="group relative px-3 py-1 mb-0.5 hover:bg-[var(--c-hover)] transition-colors duration-75"
    >
      {/* Reply thread indicator */}
      {replyTo && (
        <button
          data-testid={`reply-preview-${message.reply_to_message_id}`}
          onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
          className="flex items-center gap-1 text-xs font-mono mb-0.5 pl-14 opacity-60 hover:opacity-90 transition-opacity"
          style={{ color: "var(--c-text-muted)" }}
        >
          <Reply size={10} style={{ transform: "scaleX(-1)" }} />
          {replyToAuthor && (
            <span className="font-semibold flex-shrink-0" style={{ color: "var(--c-text-dim)" }}>
              {replyToAuthor}:
            </span>
          )}
          <span className="truncate max-w-xs">
            {replyTo.content_decrypted?.slice(0, 80) || "[encrypted]"}
          </span>
        </button>
      )}

      {/* IRC-style inline row: HH:MM  username  message */}
      <div className="flex items-baseline gap-0 min-w-0">
        <span
          data-testid="message-timestamp"
          className="flex-shrink-0 text-sm font-mono tabular-nums select-none w-12 mr-2"
          style={{ color: "var(--c-text-muted)" }}
        >
          {formatTimestamp(message.created_at)}
        </span>

        <span
          data-testid="message-author"
          className="flex-shrink-0 font-mono text-sm font-semibold mr-1"
          style={{
            color: isOwn ? "var(--c-accent)" : "var(--c-text-dim)",
          }}
        >
          {authorUsername}
        </span>

        <span
          className="font-mono text-sm select-none mr-1 flex-shrink-0"
          style={{ color: "var(--c-text-muted)" }}
          aria-hidden="true"
        >
          {":"}
        </span>

        <span
          data-testid="message-content"
          className="font-mono text-sm break-words flex-1 min-w-0"
          style={{ color: "var(--c-text)", whiteSpace: "pre-wrap" }}
        >
          {content}
          {message.status && message.status !== "sent" && (
            <span className="ml-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
              [{message.status}]
            </span>
          )}
        </span>

        {/* Reply button — only visible on hover */}
        <button
          data-testid="reply-button"
          onClick={() => onReply?.(message.id)}
          aria-label="Reply"
          className="flex-shrink-0 ml-1 opacity-0 group-hover:opacity-60 hover:!opacity-100 transition-opacity"
          style={{ color: "var(--c-text-muted)" }}
        >
          <Reply size={12} />
        </button>
      </div>

      {/* Attachments below the inline row */}
      {message.attachments && message.attachments.length > 0 && (
        <div className="mt-1 ml-11 flex flex-col gap-1">
          {message.attachments.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}

      {/* Reactions row — disabled, needs more thought */}
      {/* <MessageReactions messageId={message.id} /> */}
    </div>
  );
};

const AttachmentDisplay: React.FC<{ attachment: MessageAttachment }> = ({ attachment }) => {
  const [downloadUrl, setDownloadUrl] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const isImage = attachment.content_type.startsWith("image/");

  useEffect(() => {
    if (!isImage || downloadUrl) {
      return;
    }
    setIsLoading(true);
    getFileDownloadUrl(attachment.object_key)
      .then(setDownloadUrl)
      .catch((err) => setError(err instanceof Error ? err.message : "Failed to load"))
      .finally(() => setIsLoading(false));
  }, [attachment.object_key, isImage, downloadUrl]);

  const formatFileSize = (bytes: number) => {
    if (bytes === 0) { return "0B"; }
    const sizes = ["B", "KB", "MB", "GB"];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return `${parseFloat((bytes / Math.pow(1024, i)).toFixed(1))}${sizes[i]}`;
  };

  const handleDownload = async () => {
    const url = downloadUrl ?? await getFileDownloadUrl(attachment.object_key);
    if (!downloadUrl) { setDownloadUrl(url); }
    const a = document.createElement("a");
    a.href = url;
    a.download = attachment.filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
  };

  if (isImage && downloadUrl) {
    return (
      <div style={{ border: "1px solid var(--c-border)", maxWidth: 280, borderRadius: 4 }}>
        <img src={downloadUrl} alt={attachment.filename} className="block max-w-full" style={{ borderRadius: "4px 4px 0 0" }} />
        <div className="flex items-center justify-between px-2 py-1" style={{ background: "var(--c-surface-high)" }}>
          <span className="text-xs font-mono truncate" style={{ color: "var(--c-text-dim)" }}>{attachment.filename}</span>
          <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>{formatFileSize(attachment.file_size)}</span>
        </div>
      </div>
    );
  }

  return (
    <div
      data-testid={`attachment-${attachment.id}`}
      className="flex items-center gap-2 px-2.5 py-1.5"
      style={{ border: "1px solid var(--c-border)", background: "var(--c-surface-high)", maxWidth: 280, borderRadius: 4 }}
    >
      {isImage ? (
        <ImageIcon size={16} aria-hidden="true" style={{ color: "var(--c-text-dim)", flexShrink: 0 }} />
      ) : (
        <FileIcon size={16} aria-hidden="true" style={{ color: "var(--c-text-dim)", flexShrink: 0 }} />
      )}
      <div className="flex-1 min-w-0">
        <div className="text-xs font-mono truncate" style={{ color: "var(--c-accent-dim)" }}>{attachment.filename}</div>
        <div className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>{formatFileSize(attachment.file_size)}</div>
      </div>
      {error ? (
        <span className="text-xs" style={{ color: "var(--c-text-muted)" }}>{error}</span>
      ) : (
        <button onClick={handleDownload} disabled={isLoading} aria-label={`Download ${attachment.filename}`} style={{ color: "var(--c-text-muted)" }}>
          {isLoading ? "…" : <Download size={14} aria-hidden="true" />}
        </button>
      )}
    </div>
  );
};
