import React from "react";
import { Reply, CornerUpLeft, Edit2, Trash2 } from "lucide-react";
import { formatTimeOfDay, formatFullTimestamp } from "../../utils/format";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { LinkifiedText } from "../ui/LinkifiedText";
import { MediaLinkUnfurl } from "./MediaLinkUnfurl";
import { getUsernameColor, useBackgroundIsLight } from "../../utils/usernameColor";
import { useSkin } from "../../hooks/queries/usePreferences";
import { AttachmentDisplay } from "./AttachmentDisplay";
import { MessageAvatar } from "./MessageAvatar";
import type { Message } from "../../types";

interface MessageItemProps {
  message: Message;
  allMessages?: Message[];
  authorUsername?: string;
  isAuthorAdmin?: boolean;
  /** True when the viewer is an admin in this message's group — enables
   * deleting other members' messages for moderation. */
  canModerate?: boolean;
  /** Refined skin only: when true this row starts a new sender group and
   * renders the avatar + name/timestamp header; when false it's a grouped
   * follow-up (body only, hover-timestamp in the gutter). Computed by
   * MessageList. Terminal ignores it. Defaults to true so non-grouping
   * callers keep the full header. */
  isGroupStart?: boolean;
  onReply?: (messageId: string) => void;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  onPin?: (messageId: string) => void;
  onScrollToReply?: (messageId: string) => void;
}

// `created_at` arrives as unix seconds or milliseconds depending on source;
// normalize to milliseconds before formatting.
const toMs = (timestamp: number): number =>
  timestamp < 1e12 ? timestamp * 1000 : timestamp;

export const MessageItem: React.FC<MessageItemProps> = observer(({
  message,
  allMessages = [],
  authorUsername = "unknown",
  isAuthorAdmin = false,
  canModerate = false,
  isGroupStart = true,
  onReply,
  onEdit,
  onDelete,
  onScrollToReply,
}) => {
  const { currentUser } = appStore;
  const isOwn = message.sender_id === currentUser?.id;
  const isLightBg = useBackgroundIsLight();
  const skin = useSkin();

  // Stable per-user color for non-own, non-admin authors. Key on username
  // when available so the same person keeps the same color across groups
  // even if their user id rotates; fall back to sender_id otherwise.
  const authorColorKey = message.sender_username ?? message.sender_id;
  const authorColor = getUsernameColor(authorColorKey, isLightBg);

  const replyTo = message.reply_to_message_id
    ? allMessages.find((m) => m.id === message.reply_to_message_id)
    : null;

  const replyToAuthor = replyTo
    ? (replyTo.sender_username ?? replyTo.sender_id)
    : null;
  const replyToAuthorColor = replyTo
    ? getUsernameColor(replyTo.sender_username ?? replyTo.sender_id, isLightBg)
    : null;

  const isDeleted = !!message.deleted_at;

  // content_decrypted is undefined when decryption failed (the server returned
  // null). Show [encrypted] in that case rather than an empty row.
  const content = isDeleted ? "[deleted]" : (message.content_decrypted ?? "[encrypted]");

  // Split attachments into a visual media strip (images + videos rendered as
  // uniform 96×96 thumbs) and everything else (audio, files) which render as
  // text-aligned rows below the strip.
  const isVisualMedia = (ct: string) =>
    ct.startsWith("image/") || ct.startsWith("video/");
  const mediaThumbs = message.attachments?.filter((a) => isVisualMedia(a.content_type)) ?? [];
  const otherAttachments = message.attachments?.filter((a) => !isVisualMedia(a.content_type)) ?? [];

  // Attachment blocks — identical markup for both skins (the media strip and
  // the file/audio column). Rendered inside each skin's content region.
  const attachmentBlocks = (
    <>
      {/* Visual media: horizontal strip of uniform 96×96 thumbs */}
      {mediaThumbs.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {mediaThumbs.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}

      {/* Audio / files — each on its own row */}
      {otherAttachments.length > 0 && (
        <div className="mt-2 flex flex-col gap-2">
          {otherAttachments.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}
    </>
  );

  // ── Refined skin: Slack-style avatar-gutter row ───────────────────────────
  if (skin === "refined") {
    // Per-author name color; admins keep the accent badge treatment.
    const nameStyle: React.CSSProperties = isAuthorAdmin
      ? {
          background: "var(--c-accent)",
          color: "var(--c-bg)",
          paddingLeft: "0.25rem",
          paddingRight: "0.25rem",
          borderRadius: "var(--radius-chip)",
        }
      : { color: isOwn ? "var(--c-accent)" : authorColor };

    return (
      <div
        data-testid={`message-${message.id}`}
        aria-label={`Message from ${authorUsername}`}
        className="group relative grid grid-cols-[3.5rem_minmax(0,1fr)] gap-x-2 items-start px-4 hover:bg-hover transition-colors duration-75"
        style={{
          paddingTop: isGroupStart ? "var(--msg-header-gap)" : "var(--msg-group-gap)",
          paddingBottom: "var(--msg-row-pad-y)",
        }}
      >
        {/* Left gutter: avatar on a group start, hover-only timestamp otherwise */}
        <div className="flex justify-center pt-0.5 select-none">
          {isGroupStart ? (
            <MessageAvatar userId={message.sender_id} username={authorUsername} size={36} />
          ) : (
            <span
              title={formatFullTimestamp(toMs(message.created_at))}
              className="font-machine text-2xs tabular-nums whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity"
              style={{ color: "var(--c-text-muted)" }}
            >
              {formatTimeOfDay(toMs(message.created_at))}
            </span>
          )}
        </div>

        {/* Right column: reply ref, header, body, attachments */}
        <div className="min-w-0">
          {/* Reply reference above the body */}
          {message.reply_to_message_id && (
            replyTo ? (
              <button
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
                className="flex items-center gap-1 text-xs mb-0.5 opacity-70 hover:opacity-100 transition-opacity min-w-0"
                style={{ color: "var(--c-text-muted)" }}
              >
                <CornerUpLeft size={12} className="flex-shrink-0" />
                {replyToAuthor && (
                  <span
                    className="font-semibold flex-shrink-0"
                    style={{ color: replyToAuthorColor ?? "var(--c-text-dim)" }}
                  >
                    {replyToAuthor}:
                  </span>
                )}
                <span className="truncate max-w-xs">
                  {replyTo.content_decrypted?.slice(0, 80) || "[encrypted]"}
                </span>
              </button>
            ) : (
              <div
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                className="flex items-center gap-1 text-xs mb-0.5"
                style={{ color: "var(--c-text-dim)" }}
              >
                <CornerUpLeft size={12} className="flex-shrink-0" />
                <span>[redacted]</span>
              </div>
            )
          )}

          {/* Header: sender name + faint timestamp (group start only) */}
          {isGroupStart && (
            <div className="flex items-baseline gap-2 min-w-0">
              <span
                data-testid="message-author"
                className="text-sm font-semibold flex-shrink-0"
                style={nameStyle}
              >
                {authorUsername}
              </span>
              <span
                title={formatFullTimestamp(toMs(message.created_at))}
                className="font-machine text-2xs tabular-nums select-none flex-shrink-0"
                style={{ color: "var(--c-text-muted)" }}
              >
                {formatTimeOfDay(toMs(message.created_at))}
              </span>
            </div>
          )}

          {/* Message body */}
          <div
            data-testid="message-content"
            className="text-sm break-words"
            style={{
              color: isDeleted ? "var(--c-text-muted)" : "var(--c-text)",
              whiteSpace: "pre-wrap",
              lineHeight: "var(--lh)",
            }}
          >
            <LinkifiedText text={content} />
            {message.edited_at && !isDeleted && (
              <span className="ml-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
                (edited)
              </span>
            )}
            {message.status && message.status !== "sent" && (
              <span className="ml-1 text-xs font-machine" style={{ color: "var(--c-text-muted)" }}>
                [{message.status}]
              </span>
            )}
          </div>

          {/* Inline previews for media URLs typed in the message body */}
          {!isDeleted && <MediaLinkUnfurl text={content} />}

          {attachmentBlocks}

          {/* Reactions row — disabled, needs more thought */}
          {/* <MessageReactions messageId={message.id} /> */}
        </div>

        {/* Floating hover action toolbar */}
        {!isDeleted && (
          <div className="absolute right-4 top-0 -translate-y-1/2 opacity-0 group-hover:opacity-100 transition-opacity flex items-center gap-1 rounded-[var(--radius-control)] border border-line bg-surface-raised px-1 py-0.5">
            <button
              data-testid="reply-button"
              onClick={() => onReply?.(message.id)}
              aria-label="Reply"
              className="p-1 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
            >
              <Reply size={16} />
            </button>
            {isOwn && onEdit && (
              <button
                data-testid="edit-button"
                onClick={() => onEdit(message.id)}
                aria-label="Edit message"
                className="p-1 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
              >
                <Edit2 size={16} />
              </button>
            )}
            {isOwn && onDelete && (
              <button
                data-testid="delete-button"
                onClick={() => onDelete(message.id)}
                aria-label="Delete message"
                className="p-1 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
              >
                <Trash2 size={16} />
              </button>
            )}
            {!isOwn && canModerate && onDelete && (
              <button
                data-testid="admin-delete-button"
                onClick={() => onDelete(message.id)}
                aria-label="Delete message (admin)"
                className="p-1 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
              >
                <Trash2 size={16} />
              </button>
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div
      data-testid={`message-${message.id}`}
      aria-label={`Message from ${authorUsername}`}
      className="group relative px-4 py-1 hover:bg-[var(--c-hover)] transition-colors duration-75"
    >
      {/* Reply thread indicator */}
      {message.reply_to_message_id && (
        replyTo ? (
          <button
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
            className="flex items-center gap-1 text-xs font-mono mb-1.5 pl-14 opacity-60 hover:opacity-90 transition-opacity"
            style={{ color: "var(--c-text-muted)" }}
          >
            <Reply size={10} style={{ transform: "scaleX(-1)" }} />
            {replyToAuthor && (
              <span
                className="font-semibold flex-shrink-0"
                style={{ color: replyToAuthorColor ?? "var(--c-text-dim)" }}
              >
                {replyToAuthor}:
              </span>
            )}
            <span className="truncate max-w-xs">
              {replyTo.content_decrypted?.slice(0, 80) || "[encrypted]"}
            </span>
          </button>
        ) : (
          <div
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            className="flex items-center gap-1 text-xs font-mono mb-1.5 pl-14"
            style={{ color: "var(--c-text-dim)" }}
          >
            <Reply size={10} style={{ transform: "scaleX(-1)" }} />
            <span>[redacted]</span>
          </div>
        )
      )}

      {/* IRC-style inline row: HH:MM  username  message */}
      <div className="flex items-start gap-0 min-w-0">
        <span
          data-testid="message-timestamp"
          title={formatFullTimestamp(toMs(message.created_at))}
          className="flex-shrink-0 text-xs font-mono tabular-nums select-none w-20"
          style={{ color: "var(--c-text-muted)", lineHeight: "1.5rem" }}
        >
          {formatTimeOfDay(toMs(message.created_at))}
        </span>

        <span
          data-testid="message-author"
          className="flex-shrink-0 font-mono text-sm font-semibold mr-1"
          style={isAuthorAdmin ? {
            background: "var(--c-accent)",
            color: "var(--c-bg)",
            paddingLeft: "0.25rem",
            paddingRight: "0.25rem",
            borderRadius: "0.125rem",
          } : {
            color: isOwn ? "var(--c-accent)" : authorColor,
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
          style={{
            color: isDeleted ? "var(--c-text-muted)" : "var(--c-text)",
            whiteSpace: "pre-wrap",
          }}
        >
          <LinkifiedText text={content} />
          {message.edited_at && !isDeleted && (
            <span className="ml-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
              (edited)
            </span>
          )}
          {message.status && message.status !== "sent" && (
            <span className="ml-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
              [{message.status}]
            </span>
          )}
        </span>

        {/* Action buttons — only visible on hover */}
        {!isDeleted && (
          <div className="flex-shrink-0 ml-2 flex items-center gap-4 h-6">
            <button
              data-testid="reply-button"
              onClick={() => onReply?.(message.id)}
              aria-label="Reply"
              className="opacity-0 group-hover:opacity-100 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
            >
              <Reply size={18} />
            </button>
            {isOwn && onEdit && (
              <button
                data-testid="edit-button"
                onClick={() => onEdit(message.id)}
                aria-label="Edit message"
                className="opacity-0 group-hover:opacity-100 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
              >
                <Edit2 size={18} />
              </button>
            )}
            {isOwn && onDelete && (
              <button
                data-testid="delete-button"
                onClick={() => onDelete(message.id)}
                aria-label="Delete message"
                className="opacity-0 group-hover:opacity-100 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
              >
                <Trash2 size={18} />
              </button>
            )}
            {!isOwn && canModerate && onDelete && (
              <button
                data-testid="admin-delete-button"
                onClick={() => onDelete(message.id)}
                aria-label="Delete message (admin)"
                className="opacity-0 group-hover:opacity-100 text-[var(--c-text-muted)] hover:text-[var(--c-text-accent)]"
              >
                <Trash2 size={18} />
              </button>
            )}
          </div>
        )}
      </div>

      {/* Inline previews for media URLs typed in the message body */}
      {!isDeleted && <MediaLinkUnfurl text={content} />}

      {/* Visual media: horizontal strip of uniform 96×96 thumbs */}
      {mediaThumbs.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {mediaThumbs.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}

      {/* Audio / files — each on its own row */}
      {otherAttachments.length > 0 && (
        <div className="mt-2 flex flex-col gap-2">
          {otherAttachments.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}

      {/* Reactions row — disabled, needs more thought */}
      {/* <MessageReactions messageId={message.id} /> */}
    </div>
  );
});
