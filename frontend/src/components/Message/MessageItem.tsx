import React from "react";
import { useTranslation } from "react-i18next";
import { Reply, CornerUpLeft } from "lucide-react";
import { ThreadReplyCount } from "./ThreadReplyCount";
import { formatTimeOfDay, formatFullTimestamp } from "../../utils/format";
import { observer } from "mobx-react-lite";
import { MessageBody } from "./MessageBody";
import { MediaLinkUnfurl } from "./MediaLinkUnfurl";
import { getUsernameColor } from "../../utils/usernameColor";
import { AttachmentDisplay } from "./AttachmentDisplay";
import { MessageAvatar } from "./MessageAvatar";
import { MessageActions } from "./MessageActions";
import { ReceiptIndicator } from "./ReceiptIndicator";
import { probeRender } from "../../utils/renderProbe";
import type { MessageRenderContext } from "./messageRenderContext";
import type { Message, MessageReceipts } from "../../types";

interface MessageItemProps {
  message: Message;
  /** List-wide render inputs (skin, viewer, mention roster). One memoised
   *  object from `MessageList` — see `messageRenderContext.ts`. */
  ctx: MessageRenderContext;
  /** The message this one replies to, already resolved by the list. Was a
   *  `allMessages.find()` per row, i.e. O(N^2) over the whole log (#874).
   *  `undefined` = no reply; `null` = replied to something not loaded. */
  replyToMessage?: Message | null;
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
  /** Open this message's thread in the right panel (#825). */
  onOpenThread?: (messageId: string) => void;
  /** Replies this device holds for this message's thread, 0 if none. */
  threadReplyCount?: number;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  /** Bookmarks + permalinks (#854). All optional — when the parent doesn't
   * pass them the affordances simply don't render. Wired from `MessageList`. */
  onToggleSave?: (messageId: string) => void;
  onCopyLink?: (messageId: string) => void;
  /** Transient outcome of THIS message's last copy-link click (#889).
   *  `idle` for every message but the one just acted on. */
  copyLinkState?: "idle" | "copied" | "failed";
  isSaved?: boolean;
  onScrollToReply?: (messageId: string) => void;
  /** Delivery / read receipts for THIS message (#857). Optional — when the
   * parent doesn't pass it no indicator renders. Wired from `MessageList`;
   * receipts exist for DMs only, so `isDm` gates the whole affordance.
   * #874: this used to be the list's whole `Map`, whose identity changes on
   * any peer's receipt — which re-rendered every row in the log to update one. */
  receipt?: MessageReceipts;
  /** DM members excluding the viewer — drives the "2/4" multi-reader summary. */
  peerCount?: number;
  isDm?: boolean;
}

// `created_at` arrives as unix seconds or milliseconds depending on source;
// normalize to milliseconds before formatting.
const toMs = (timestamp: number): number =>
  timestamp < 1e12 ? timestamp * 1000 : timestamp;

export const MessageItem: React.FC<MessageItemProps> = observer(({
  message,
  ctx,
  replyToMessage,
  authorUsername,
  isAuthorAdmin = false,
  canModerate = false,
  isGroupStart = true,
  onReply,
  onOpenThread,
  threadReplyCount = 0,
  onEdit,
  onDelete,
  onToggleSave,
  onCopyLink,
  copyLinkState = "idle",
  isSaved = false,
  onScrollToReply,
  receipt,
  peerCount = 0,
  isDm = false,
}) => {
  probeRender("MessageItem");
  const { t } = useTranslation("chat");
  const { skin, isLightBg, currentUserId } = ctx;
  const isOwn = message.sender_id === currentUserId;
  // Falls back to the same placeholder the default prop used to supply, but
  // resolved at render so it follows the active language.
  const displayName = authorUsername ?? t("message.unknownAuthor");
  // Stable per-user color for non-own, non-admin authors. Key on username
  // when available so the same person keeps the same color across groups
  // even if their user id rotates; fall back to sender_id otherwise.
  const authorColorKey = message.sender_username ?? message.sender_id;
  const authorColor = getUsernameColor(authorColorKey, isLightBg);

  const replyTo = replyToMessage ?? null;

  const replyToAuthor = replyTo
    ? (replyTo.sender_username ?? replyTo.sender_id)
    : null;
  const replyToAuthorColor = replyTo
    ? getUsernameColor(replyTo.sender_username ?? replyTo.sender_id, isLightBg)
    : null;

  const isDeleted = !!message.deleted_at;

  // content_decrypted is undefined when decryption failed (the server returned
  // null). Show [encrypted] in that case rather than an empty row.
  const content = isDeleted
    ? t("message.deleted")
    : (message.content_decrypted ?? t("message.encrypted"));

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
          paddingInlineStart: "0.25rem",
          paddingInlineEnd: "0.25rem",
          borderRadius: "var(--radius-chip)",
        }
      : { color: isOwn ? "var(--c-accent)" : authorColor };

    return (
      <div
        data-testid={`message-${message.id}`}
        aria-label={t("message.fromLabel", { name: displayName })}
        // tabIndex -1: arrow-key log navigation (messageNavStore) focuses the
        // row programmatically; focus-within mirrors the hover treatment so a
        // keyboard-focused row (or its action bar) reads exactly like a
        // hovered one, with zero per-keystroke re-render.
        tabIndex={-1}
        className="group relative grid grid-cols-[3.5rem_minmax(0,1fr)] gap-x-2 items-start px-4 hover:bg-hover focus-within:bg-hover outline-none transition-colors duration-75"
        style={{
          paddingTop: isGroupStart ? "var(--msg-header-gap)" : "var(--msg-group-gap)",
          paddingBottom: "var(--msg-row-pad-y)",
        }}
      >
        {/* Left gutter: avatar on a group start, hover-only timestamp otherwise */}
        <div className="flex justify-center pt-0.5 select-none">
          {isGroupStart ? (
            <MessageAvatar userId={message.sender_id} username={displayName} size={36} />
          ) : (
            <bdi
              dir="ltr"
              title={formatFullTimestamp(toMs(message.created_at))}
              className="font-machine text-2xs tabular-nums whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity"
              style={{ color: "var(--c-text-muted)" }}
            >
              {formatTimeOfDay(toMs(message.created_at))}
            </bdi>
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
                <CornerUpLeft size={12} className="flex-shrink-0 rtl-mirror" />
                {replyToAuthor && (
                  <bdi
                    className="font-semibold flex-shrink-0"
                    style={{ color: replyToAuthorColor ?? "var(--c-text-dim)" }}
                  >
                    {replyToAuthor}:
                  </bdi>
                )}
                <span className="truncate max-w-xs" dir="auto">
                  {replyTo.content_decrypted?.slice(0, 80) || t("message.encrypted")}
                </span>
              </button>
            ) : (
              <div
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                className="flex items-center gap-1 text-xs mb-0.5"
                style={{ color: "var(--c-text-dim)" }}
              >
                <CornerUpLeft size={12} className="flex-shrink-0 rtl-mirror" />
                <span>{t("message.redacted")}</span>
              </div>
            )
          )}

          {/* Header: sender name + faint timestamp (group start only) */}
          {isGroupStart && (
            <div className="flex items-baseline gap-2 min-w-0">
              <bdi
                data-testid="message-author"
                className="text-sm font-semibold flex-shrink-0"
                style={nameStyle}
              >
                {displayName}
              </bdi>
              <bdi
                data-testid="message-timestamp"
                dir="ltr"
                title={formatFullTimestamp(toMs(message.created_at))}
                className="font-machine text-2xs tabular-nums select-none flex-shrink-0"
                style={{ color: "var(--c-text-muted)" }}
              >
                {formatTimeOfDay(toMs(message.created_at))}
              </bdi>
            </div>
          )}

          {/* Message body */}
          <div
            data-testid="message-content"
            dir="auto"
            className="text-sm break-words"
            style={{
              color: isDeleted ? "var(--c-text-muted)" : "var(--c-text)",
              whiteSpace: "pre-wrap",
              lineHeight: "var(--lh)",
            }}
          >
            <MessageBody text={content} ctx={ctx} />
            {message.edited_at && !isDeleted && (
              <span className="ms-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
                {t("message.edited")}
              </span>
            )}
            {message.status && message.status !== "sent" && (
              <span className="ms-1 text-xs font-machine" style={{ color: "var(--c-text-muted)" }}>
                [{t(`status.${message.status}`)}]
              </span>
            )}
            <ReceiptIndicator
              receipts={receipt}
              peerCount={peerCount}
              visible={isOwn && isDm}
            />
          </div>

          {/* Inline previews for media URLs typed in the message body */}
          {!isDeleted && <MediaLinkUnfurl text={content} />}

          {attachmentBlocks}

          <ThreadReplyCount
            count={threadReplyCount}
            onOpen={onOpenThread ? () => onOpenThread(message.id) : undefined}
          />
        </div>

        {/* Floating hover action toolbar */}
        {!isDeleted && (
          <MessageActions
            messageId={message.id}
            variant="refined"
            isOwn={isOwn}
            canModerate={canModerate}
            isSaved={isSaved}
            copyLinkState={copyLinkState}
            onReply={onReply}
            onOpenThread={onOpenThread}
            onToggleSave={onToggleSave}
            onCopyLink={onCopyLink}
            onEdit={onEdit}
            onDelete={onDelete}
          />
        )}
      </div>
    );
  }

  return (
    <div
      data-testid={`message-${message.id}`}
      aria-label={t("message.fromLabel", { name: displayName })}
      // tabIndex -1 + focus-within: see the refined row above.
      tabIndex={-1}
      className="group relative px-4 py-1 hover:bg-[var(--c-hover)] focus-within:bg-[var(--c-hover)] outline-none transition-colors duration-75"
    >
      {/* Reply thread indicator */}
      {message.reply_to_message_id && (
        replyTo ? (
          <button
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
            className="flex items-center gap-1 text-xs font-mono mb-1.5 ps-14 opacity-60 hover:opacity-90 transition-opacity"
            style={{ color: "var(--c-text-muted)" }}
          >
            <Reply size={10} className="rtl-unmirror" />
            {replyToAuthor && (
              <bdi
                className="font-semibold flex-shrink-0"
                style={{ color: replyToAuthorColor ?? "var(--c-text-dim)" }}
              >
                {replyToAuthor}:
              </bdi>
            )}
            <span className="truncate max-w-xs" dir="auto">
              {replyTo.content_decrypted?.slice(0, 80) || t("message.encrypted")}
            </span>
          </button>
        ) : (
          <div
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            className="flex items-center gap-1 text-xs font-mono mb-1.5 ps-14"
            style={{ color: "var(--c-text-dim)" }}
          >
            <Reply size={10} className="rtl-unmirror" />
            <span>{t("message.redacted")}</span>
          </div>
        )
      )}

      {/* IRC-style inline row: HH:MM  username  message */}
      <div className="flex items-start gap-0 min-w-0">
        <bdi
          data-testid="message-timestamp"
          dir="ltr"
          title={formatFullTimestamp(toMs(message.created_at))}
          className="flex-shrink-0 text-xs font-mono tabular-nums select-none w-20"
          style={{ color: "var(--c-text-muted)", lineHeight: "1.5rem" }}
        >
          {formatTimeOfDay(toMs(message.created_at))}
        </bdi>

        <bdi
          data-testid="message-author"
          className="flex-shrink-0 font-mono text-sm font-semibold me-1"
          style={isAuthorAdmin ? {
            background: "var(--c-accent)",
            color: "var(--c-bg)",
            paddingInlineStart: "0.25rem",
            paddingInlineEnd: "0.25rem",
            borderRadius: "0.125rem",
          } : {
            color: isOwn ? "var(--c-accent)" : authorColor,
          }}
        >
          {displayName}
        </bdi>

        <span
          className="font-mono text-sm select-none me-1 flex-shrink-0"
          style={{ color: "var(--c-text-muted)" }}
          aria-hidden="true"
        >
          {":"}
        </span>

        <span
          data-testid="message-content"
          dir="auto"
          className="font-mono text-sm break-words flex-1 min-w-0"
          style={{
            color: isDeleted ? "var(--c-text-muted)" : "var(--c-text)",
            whiteSpace: "pre-wrap",
          }}
        >
          <MessageBody text={content} ctx={ctx} />
          {message.edited_at && !isDeleted && (
            <span className="ms-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
              {t("message.edited")}
            </span>
          )}
          {message.status && message.status !== "sent" && (
            <span className="ms-1 text-xs" style={{ color: "var(--c-text-muted)" }}>
              [{t(`status.${message.status}`)}]
            </span>
          )}
          <ReceiptIndicator
            receipts={receipt}
            peerCount={peerCount}
            visible={isOwn && isDm}
          />
        </span>

        {/* Action buttons — only visible on hover */}
        {!isDeleted && (
          <MessageActions
            messageId={message.id}
            variant="terminal"
            isOwn={isOwn}
            canModerate={canModerate}
            isSaved={isSaved}
            copyLinkState={copyLinkState}
            onReply={onReply}
            onOpenThread={onOpenThread}
            onToggleSave={onToggleSave}
            onCopyLink={onCopyLink}
            onEdit={onEdit}
            onDelete={onDelete}
          />
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
    </div>
  );
});
