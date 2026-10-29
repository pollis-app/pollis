import React from "react";
import { useTranslation } from "react-i18next";
import { Reply, CornerUpLeft } from "lucide-react";
import { ThreadReplyCount } from "./ThreadReplyCount";
import { formatTimeOfDay, formatFullTimestamp, toMs } from "../../utils/format";
import { observer } from "mobx-react-lite";
import { MessageBody } from "./MessageBody";
import { MediaLinkUnfurl } from "./MediaLinkUnfurl";
import { getUsernameColor, getUsernameChipColor } from "../../utils/usernameColor";
import { AttachmentDisplay } from "./AttachmentDisplay";
import { MessageAvatar } from "./MessageAvatar";
import { MessageActions } from "./MessageActions";
import { MessageReactions } from "./MessageReactions";
import { ReceiptIndicator } from "./ReceiptIndicator";
import { UndecryptableMessage } from "./UndecryptableMessage";
import { UnsentMessageActions } from "./UnsentMessageActions";
import { probeRender } from "../../utils/renderProbe";
import type { MessageRenderContext } from "./messageRenderContext";
import type { Message, MessageReceipts } from "../../types";
import { PressButton } from "../ui/PressButton";

interface MessageItemProps {
  message: Message;
  /** List-wide render inputs; see `messageRenderContext.ts`. */
  ctx: MessageRenderContext;
  /** Resolved by the list (#874). `undefined` = no reply; `null` = not loaded. */
  replyToMessage?: Message | null;
  authorUsername?: string;
  isAuthorAdmin?: boolean;
  /** Author joined within a week (#1506): draws the "new" tag. */
  isAuthorNew?: boolean;
  /** Viewer is a group admin: may delete others' messages. */
  canModerate?: boolean;
  /** Refined skin: row starts a sender group (avatar + header). Defaults to true. */
  isGroupStart?: boolean;
  onReply?: (messageId: string) => void;
  onOpenThread?: (messageId: string) => void;
  threadReplyCount?: number;
  onEdit?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  /** Bookmarks + permalinks (#854); absent props hide the affordance. */
  onToggleSave?: (messageId: string) => void;
  onCopyLink?: (messageId: string) => void;
  onReport?: (messageId: string) => void;
  onTogglePin?: (messageId: string) => void;
  /** Outcome of this message's last copy-link click (#889). */
  copyLinkState?: "idle" | "copied" | "failed";
  isSaved?: boolean;
  isPinned?: boolean;
  onScrollToReply?: (messageId: string) => void;
  /** Receipts for this message only (#857); refined skin, DMs only. */
  receipt?: MessageReceipts;
  peerCount?: number;
  isDm?: boolean;
}

export const MessageItem: React.FC<MessageItemProps> = observer(({
  message,
  ctx,
  replyToMessage,
  authorUsername,
  isAuthorAdmin = false,
  isAuthorNew = false,
  canModerate = false,
  isGroupStart = true,
  onReply,
  onOpenThread,
  threadReplyCount = 0,
  onEdit,
  onDelete,
  onToggleSave,
  onTogglePin,
  onCopyLink,
  onReport,
  copyLinkState = "idle",
  isSaved = false,
  isPinned = false,
  onScrollToReply,
  receipt,
  peerCount = 0,
  isDm = false,
}) => {
  probeRender("MessageItem");
  const { t } = useTranslation("chat");
  const { skin, isLightBg, currentUserId } = ctx;
  if (message.undecryptable) {
    return <UndecryptableMessage message={message} skin={skin} />;
  }
  const isOwn = message.sender_id === currentUserId;
  // Not delivered yet (#1335): no reply/react/pin/edit until it is.
  const isUnsent = !!message.outbox_id || message.id.startsWith("pending-");
  const displayName = authorUsername ?? t("message.unknownAuthor");
  const authorColorKey = message.sender_username ?? message.sender_id;
  const authorColor = getUsernameColor(authorColorKey, isLightBg);
  // Admin chip (#1488): own keeps the accent, others their stable colour.
  const adminChipClass = isOwn ? "bg-accent text-bg" : "text-ink";
  const adminChipStyle: React.CSSProperties | undefined = isOwn
    ? undefined
    : { background: getUsernameChipColor(authorColorKey) };

  const replyTo = replyToMessage ?? null;

  // The "new" tag (#1506): refined skin only.
  const newTag = isAuthorNew ? (
    <span
      data-testid="message-new-tag"
      title={t("message.newMemberTitle")}
      className="flex-shrink-0 self-center px-1.5 rounded-chip bg-surface-high text-accent text-2xs font-medium select-none"
    >
      {t("message.newMember")}
    </span>
  ) : null;

  const replyToAuthor = replyTo
    ? (replyTo.sender_username ?? replyTo.sender_id)
    : null;
  const replyToAuthorColor = replyTo
    ? getUsernameColor(replyTo.sender_username ?? replyTo.sender_id, isLightBg)
    : null;

  const isDeleted = !!message.deleted_at;

  const content = isDeleted
    ? t("message.deleted")
    : (message.content_decrypted ?? t("message.encrypted"));

  const isVisualMedia = (ct: string) =>
    ct.startsWith("image/") || ct.startsWith("video/");
  const mediaThumbs = message.attachments?.filter((a) => isVisualMedia(a.content_type)) ?? [];
  const otherAttachments = message.attachments?.filter((a) => !isVisualMedia(a.content_type)) ?? [];

  const attachmentBlocks = (
    <>
      {mediaThumbs.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {mediaThumbs.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}

      {otherAttachments.length > 0 && (
        <div className="mt-2 flex flex-col gap-2">
          {otherAttachments.map((a) => (
            <AttachmentDisplay key={a.id} attachment={a} />
          ))}
        </div>
      )}
    </>
  );

  if (skin === "refined") {
    const nameClass = isAuthorAdmin ? `${adminChipClass} px-1 rounded-chip` : "";
    const nameStyle: React.CSSProperties | undefined = isAuthorAdmin
      ? adminChipStyle
      : { color: isOwn ? "var(--c-accent)" : authorColor };

    return (
      <div
        data-testid={`message-${message.id}`}
        aria-label={t("message.fromLabel", { name: displayName })}
        // Focused programmatically by arrow-key navigation; focus-within mirrors hover.
        tabIndex={-1}
        className={`group relative grid grid-cols-[3.5rem_minmax(0,1fr)] gap-x-2 items-start px-4 ${isGroupStart ? "pt-msg-header" : "pt-msg-group"} pb-msg-row hover:bg-hover focus-within:bg-hover outline-none`}
      >
        <div className="flex justify-center pt-0.5 select-none">
          {isGroupStart ? (
            <MessageAvatar userId={message.sender_id} username={displayName} size={36} />
          ) : (
            <bdi
              dir="ltr"
              title={formatFullTimestamp(toMs(message.created_at))}
              className="font-machine text-2xs tabular-nums whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity text-muted"
            >
              {formatTimeOfDay(toMs(message.created_at))}
            </bdi>
          )}
        </div>

        <div className="min-w-0">
          {message.reply_to_message_id && (
            replyTo ? (
              <PressButton
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
                className="flex items-center gap-1 text-xs mb-0.5 opacity-70 hover:opacity-100 transition-opacity min-w-0 text-muted"
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
              </PressButton>
            ) : (
              <div
                data-testid={`reply-preview-${message.reply_to_message_id}`}
                className="flex items-center gap-1 text-xs mb-0.5 text-dim"
              >
                <CornerUpLeft size={12} className="flex-shrink-0 rtl-mirror" />
                <span>{t("message.redacted")}</span>
              </div>
            )
          )}

          {isGroupStart && (
            <div className="flex items-baseline gap-2 min-w-0">
              <bdi
                data-testid="message-author"
                className={`text-sm font-semibold flex-shrink-0 ${nameClass}`}
                style={nameStyle}
              >
                {displayName}
              </bdi>
              {newTag}
              <bdi
                data-testid="message-timestamp"
                dir="ltr"
                title={formatFullTimestamp(toMs(message.created_at))}
                className="font-machine text-2xs tabular-nums select-none flex-shrink-0 text-muted"
              >
                {formatTimeOfDay(toMs(message.created_at))}
              </bdi>
            </div>
          )}

          <div
            data-testid="message-content"
            dir="auto"
            className={`text-sm break-words leading-msg ${isDeleted ? "text-muted" : "text-fg"}`}
            style={{
              whiteSpace: "pre-wrap",
            }}
          >
            <MessageBody text={content} ctx={ctx} />
            {message.edited_at && !isDeleted && (
              <span
                className="ms-1 text-xs text-muted"
                data-testid={message.edit_unsent ? "message-edit-unsent" : undefined}
              >
                {message.edit_unsent ? t("message.editUnsent") : t("message.edited")}
              </span>
            )}
            {/* Only a failed send is labelled. */}
            {message.status === "failed" && (
              <span className="ms-1 text-xs font-machine text-muted">
                [{t(`status.${message.status}`)}]
              </span>
            )}
            {message.status === "failed" && isOwn && (
              <UnsentMessageActions message={message} />
            )}
            <ReceiptIndicator
              receipts={receipt}
              peerCount={peerCount}
              visible={isOwn && isDm}
            />
          </div>

          {!isDeleted && <MediaLinkUnfurl text={content} />}

          {attachmentBlocks}

          <ThreadReplyCount
            count={threadReplyCount}
            onOpen={onOpenThread ? () => onOpenThread(message.id) : undefined}
          />

          {/* Inside the body column so pills align with the text. */}
          {!isDeleted && <MessageReactions messageId={message.id} className="mt-1" />}
        </div>

        {!isDeleted && !isUnsent && (
          <MessageActions
            messageId={message.id}
            variant="refined"
            isOwn={isOwn}
            canModerate={canModerate}
            isSaved={isSaved}
            isPinned={isPinned}
            copyLinkState={copyLinkState}
            onReply={onReply}
            onOpenThread={onOpenThread}
            onToggleSave={onToggleSave}
            onTogglePin={onTogglePin}
            onCopyLink={onCopyLink}
            onReport={onReport}
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
      tabIndex={-1}
      className="group relative px-4 py-1 hover:bg-hover focus-within:bg-hover outline-none"
    >
      {message.reply_to_message_id && (
        replyTo ? (
          <PressButton
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            onClick={() => onScrollToReply?.(message.reply_to_message_id!)}
            className="flex items-center gap-1 text-xs font-mono mb-1.5 ps-14 opacity-60 hover:opacity-90 transition-opacity text-muted"
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
          </PressButton>
        ) : (
          <div
            data-testid={`reply-preview-${message.reply_to_message_id}`}
            className="flex items-center gap-1 text-xs font-mono mb-1.5 ps-14 text-dim"
          >
            <Reply size={10} className="rtl-unmirror" />
            <span>{t("message.redacted")}</span>
          </div>
        )
      )}

      <div className="flex items-start gap-0 min-w-0">
        <bdi
          data-testid="message-timestamp"
          dir="ltr"
          title={formatFullTimestamp(toMs(message.created_at))}
          className="flex-shrink-0 text-xs font-mono tabular-nums select-none w-20 text-muted"
          style={{ lineHeight: "1.5rem" }}
        >
          {formatTimeOfDay(toMs(message.created_at))}
        </bdi>

        <bdi
          data-testid="message-author"
          className={`flex-shrink-0 font-mono text-sm font-semibold me-1 ${isAuthorAdmin ? `${adminChipClass} px-1 rounded-sm` : ""}`}
          style={isAuthorAdmin ? adminChipStyle : {
            color: isOwn ? "var(--c-accent)" : authorColor,
          }}
        >
          {displayName}
        </bdi>

        <span
          className="font-mono text-sm select-none me-1 flex-shrink-0 text-muted"
          aria-hidden="true"
        >
          {":"}
        </span>

        {/* Body column (#1489): media aligns with the text, not the timestamp. */}
        <div className="flex-1 min-w-0">
          <span
            data-testid="message-content"
            dir="auto"
            className={`block font-mono text-sm break-words ${isDeleted ? "text-muted" : "text-fg"}`}
            style={{
              whiteSpace: "pre-wrap",
            }}
          >
            <MessageBody text={content} ctx={ctx} />
            {message.edited_at && !isDeleted && (
              <span
                className="ms-1 text-xs text-muted"
                data-testid={message.edit_unsent ? "message-edit-unsent" : undefined}
              >
                {message.edit_unsent ? t("message.editUnsent") : t("message.edited")}
              </span>
            )}
            {/* Only a failed send is labelled. */}
            {message.status === "failed" && (
              <span className="ms-1 text-xs text-muted">
                [{t(`status.${message.status}`)}]
              </span>
            )}
            {message.status === "failed" && isOwn && (
              <UnsentMessageActions message={message} />
            )}
            {/* No receipts in the terminal skin (product call). */}
          </span>

          {!isDeleted && <MediaLinkUnfurl text={content} />}

          {attachmentBlocks}

          {!isDeleted && <MessageReactions messageId={message.id} className="mt-0.5" />}
        </div>

        {!isDeleted && !isUnsent && (
          <MessageActions
            messageId={message.id}
            variant="terminal"
            isOwn={isOwn}
            canModerate={canModerate}
            isSaved={isSaved}
            isPinned={isPinned}
            copyLinkState={copyLinkState}
            onReply={onReply}
            onOpenThread={onOpenThread}
            onToggleSave={onToggleSave}
            onTogglePin={onTogglePin}
            onCopyLink={onCopyLink}
            onReport={onReport}
            onEdit={onEdit}
            onDelete={onDelete}
          />
        )}
      </div>
    </div>
  );
});
